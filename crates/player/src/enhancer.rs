//! Psychoacoustic enhancer: builds an mpv `af` fragment from ffmpeg filters.
//!
//! No PCM is touched here. mpv decodes and renders; this only constructs the
//! filter string that [`crate::Player`] appends to its existing
//! gain + pitch chain. Default is OFF, so the empty string keeps the
//! filterless path bit-identical.
//!
//! Option names verified against ffmpeg docs (do not rename without probing):
//! - `aformat=sample_fmts=fltp` (aformat: sample_fmts/sample_rates/channel_layouts)
//! - `highpass=frequency=20:poles=2` (highpass: frequency/f, poles/p)
//! - `bass=gain=..:frequency=..`, `treble=gain=..:frequency=..` (biquads family)
//! - `equalizer=frequency=..:width_type=q:width=..:gain=..` (equalizer: f/t/w/g)
//! - `aexciter=amount:drive:blend:freq:ceil` (amount 0-64 dflt 1, drive
//!   0.1-10 dflt 8.5, blend -10..10 dflt 0, freq 2000-12000 dflt 7500,
//!   ceil 9999-20000)
//! - `virtualbass=cutoff:strength` (cutoff 100-500 dflt 250, strength 0.5-3)
//! - `crossfeed=strength:range` (strength 0-1 dflt 0.2, range 0-1 dflt 0.5)
//! - `stereotools=slev=..` (slev side level, dflt 1)
//! - `aecho=in_gain:out_gain:delays:decays`
//!   (e.g. `aecho=0.8:0.88:60:0.4`; multi `1000|1800:0.3|0.25`)
//! - `asoftclip=type=tanh:threshold=..:output=1`
//!   (type tanh/atan/cubic/..., threshold 1e-6..1, output ..16)
//! - `alimiter=limit:level:asc:latency` (limit 0.0625-1, level auto bool,
//!   asc bool, latency bool). Existing code uses `level=disabled`, kept here.
//! - `volume=..dB` trim for level matching.
//!
//! mpv `af` entry syntax: `[@label:]filter[=params]`, params joined by `:`,
//! filters joined by `,`. Labels (`@enh_loud_bass:`) let the player try an
//! in-place `af-command` retune first; when mpv refuses it (direct filters,
//! not `lavfi`) the player falls back to a throttled chain rebuild.

/// Headphone vs speaker stereo handling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnhancerOutput {
    Headphones,
    Speakers,
}

impl EnhancerOutput {
    /// Parse the persisted `enhancer_output` value. Unknown -> Speakers (safer:
    /// no crossfeed surprise).
    pub fn parse(s: &str) -> Self {
        match s {
            "headphones" => EnhancerOutput::Headphones,
            _ => EnhancerOutput::Speakers,
        }
    }
}

/// Preset names. Default is Subtle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnhancerPresetName {
    Subtle,
    Warm,
    Wide,
    Reference,
}

impl EnhancerPresetName {
    pub fn parse(s: &str) -> Self {
        match s {
            "Warm" => EnhancerPresetName::Warm,
            "Wide" => EnhancerPresetName::Wide,
            "Reference" => EnhancerPresetName::Reference,
            _ => EnhancerPresetName::Subtle,
        }
    }
}

/// Per-stage amounts, each 0.0..=1.0. 0 skips the stage.
#[derive(Clone, Copy, Debug)]
pub struct EnhancerAmounts {
    pub loudness: f32,
    pub polish: f32,
    pub exciter: f32,
    pub virtual_bass: f32,
    pub stereo: f32,
    pub room: f32,
}

impl EnhancerAmounts {
    fn clamp01(v: f32) -> f32 {
        if !v.is_finite() {
            return 0.0;
        }
        v.clamp(0.0, 1.0)
    }

    /// Clamp every field into 0..=1, mapping NaN/inf to 0 (never panic,
    /// never emit NaN into the filter string).
    pub fn sanitized(self) -> Self {
        Self {
            loudness: Self::clamp01(self.loudness),
            polish: Self::clamp01(self.polish),
            exciter: Self::clamp01(self.exciter),
            virtual_bass: Self::clamp01(self.virtual_bass),
            stereo: Self::clamp01(self.stereo),
            room: Self::clamp01(self.room),
        }
    }

    /// Preset defaults. Reference is almost transparent: loudness comp only,
    /// everything else 0 (limiter + trim still apply when enabled).
    pub fn from_preset(p: EnhancerPresetName) -> Self {
        match p {
            EnhancerPresetName::Subtle => Self {
                loudness: 0.6,
                polish: 0.4,
                exciter: 0.25,
                virtual_bass: 0.3,
                stereo: 0.4,
                room: 0.0,
            },
            EnhancerPresetName::Warm => Self {
                loudness: 0.7,
                polish: 0.3,
                exciter: 0.15,
                virtual_bass: 0.6,
                stereo: 0.3,
                room: 0.15,
            },
            EnhancerPresetName::Wide => Self {
                loudness: 0.5,
                polish: 0.5,
                exciter: 0.35,
                virtual_bass: 0.25,
                stereo: 0.8,
                room: 0.1,
            },
            EnhancerPresetName::Reference => Self {
                loudness: 0.5,
                polish: 0.0,
                exciter: 0.0,
                virtual_bass: 0.0,
                stereo: 0.0,
                room: 0.0,
            },
        }
    }
}

/// Full enhancer state (persisted via `set_setting`, see Phase 3).
#[derive(Clone, Copy, Debug)]
pub struct EnhancerSettings {
    pub enabled: bool,
    /// A/B bypass. When true the builder returns empty (same loudness,
    /// same topology class) so toggling never jumps in level.
    pub bypass: bool,
    pub preset: EnhancerPresetName,
    pub amounts: EnhancerAmounts,
    pub output: EnhancerOutput,
}

impl Default for EnhancerSettings {
    /// Default state: OFF so existing behavior is unchanged. Preset Subtle,
    /// output Speakers (no crossfeed surprise), bypass false.
    fn default() -> Self {
        Self {
            enabled: false,
            bypass: false,
            preset: EnhancerPresetName::Subtle,
            amounts: EnhancerAmounts::from_preset(EnhancerPresetName::Subtle),
            output: EnhancerOutput::Speakers,
        }
    }
}

/// Which ffmpeg filters the bundled libmpv actually accepts. Probed at
/// startup by trying to set each via `af` (Phase 3); default is all true
/// for unit tests and for builds where probing has not run yet.
///
/// If a filter is missing its stage is dropped (or the documented fallback
/// is used). If `alimiter` is missing the whole chain is dropped: without
/// the safety limiter clipping cannot be ruled out.
#[derive(Clone, Copy, Debug)]
pub struct FilterCaps {
    pub aformat: bool,
    pub highpass: bool,
    pub bass: bool,
    pub treble: bool,
    pub equalizer: bool,
    pub aexciter: bool,
    pub virtualbass: bool,
    pub crossfeed: bool,
    pub stereotools: bool,
    pub aecho: bool,
    pub alimiter: bool,
    pub asoftclip: bool,
}

impl Default for FilterCaps {
    fn default() -> Self {
        Self::all()
    }
}

impl FilterCaps {
    pub fn all() -> Self {
        Self {
            aformat: true,
            highpass: true,
            bass: true,
            treble: true,
            equalizer: true,
            aexciter: true,
            virtualbass: true,
            crossfeed: true,
            stereotools: true,
            aecho: true,
            alimiter: true,
            asoftclip: true,
        }
    }

    pub fn none() -> Self {
        Self {
            aformat: false,
            highpass: false,
            bass: false,
            treble: false,
            equalizer: false,
            aexciter: false,
            virtualbass: false,
            crossfeed: false,
            stereotools: false,
            aecho: false,
            alimiter: false,
            asoftclip: false,
        }
    }
}

/// Loudness-neutral trim per preset (dB, applied at the end via `volume`).
///
/// Calibrated 2026-10-07 with ffmpeg 8.1 `ebur128` (integrated loudness) at
/// volume 80 over three 12 s stereo signals — uncorrelated pink noise, a
/// loud detuned chord mix (I=-8 LUFS) and the same chord at -15 dB:
///
/// | preset    | pink | loud | quiet | mean | trim |
/// |-----------|------|------|-------|------|------|
/// | Subtle    | +0.6 | +0.1 |  +0.8 | +0.5 | -0.5 |
/// | Warm      | +0.5 | -0.2 |  +0.4 | +0.2 | -0.2 |
/// | Wide      | +0.9 | +0.1 |  +0.8 | +0.6 | -0.6 |
/// | Reference | -0.3 | -0.6 |   0.0 | -0.3 | +0.3 |
///
/// Residuals after trim are all within 0.5 dB per signal. Two fixes fell out
/// of the measurement: `aecho` unity I/O (its 0.8/0.9 gains cost ~2.8 dB)
/// and `crossfeed` unity I/O (its level_in=0.9 default costs ~1.7 dB on wide
/// material). Headphones mode stays wider-tolerance by nature — crossfeed is
/// signal-dependent — see docs/psychoacoustic-enhancer.md.
pub fn preset_trim_db(preset: EnhancerPresetName) -> f32 {
    match preset {
        EnhancerPresetName::Subtle => -0.5,
        EnhancerPresetName::Warm => -0.2,
        EnhancerPresetName::Wide => -0.6,
        EnhancerPresetName::Reference => 0.3,
    }
}

/// Equal-loudness compensation (dB) for the app volume 0..100.
///
/// `t = 1 - vol/100` (0 at full, 1 at mute); boost grows as `t^1.5` so the
/// top of the slider stays untouched and quiet listening gets help.
/// Scaled by the stage amount. Bass capped ~6 dB, treble ~4 dB.
pub fn loudness_compensation_db(volume_percent: i64, amount: f32) -> (f32, f32) {
    let a = EnhancerAmounts::clamp01(amount);
    if a <= 0.0 {
        return (0.0, 0.0);
    }
    let v = volume_percent.clamp(0, 100) as f32 / 100.0;
    let t = (1.0 - v).clamp(0.0, 1.0);
    let curve = t.powf(1.5);
    (6.0 * curve * a, 4.0 * curve * a)
}

/// Round a gain to 1 dB steps. Non-finite in → 0.0 out (never NaN in `af`).
/// The loudness shelves move in these steps so a volume drag retunes a few
/// times instead of every frame, and equal steps compare equal so the player
/// can skip the `af` rebuild entirely when nothing audible changed.
pub fn quantize_1db(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    v.round()
}

fn fmt_db(v: f32) -> String {
    // One decimal, never NaN/inf (callers sanitize first). `-0.0` prints as
    // `0.0` so mpv never sees a signed zero gain.
    let mut x = v;
    if !x.is_finite() {
        x = 0.0;
    }
    if x.abs() < 0.0005 {
        x = 0.0;
    }
    format!("{x:.1}")
}

/// Build the enhancer `af` fragment (filters joined by `,`, empty when off).
/// Never panics, never allocates on the audio thread (this runs on the
/// caller thread before `set_property`), never emits NaN/inf.
pub fn build_enhancer_af(
    settings: &EnhancerSettings,
    volume_percent: i64,
    caps: &FilterCaps,
) -> String {
    if !settings.enabled || settings.bypass {
        return String::new();
    }
    // Without the safety limiter clipping cannot be ruled out: drop all.
    if !caps.alimiter {
        return String::new();
    }
    let amt = settings.amounts.sanitized();
    let mut parts: Vec<String> = Vec::new();

    // 0. Float precision first.
    if caps.aformat {
        parts.push("aformat=sample_fmts=fltp".to_owned());
    }
    // 1. Subsonic rumble out.
    if caps.highpass {
        parts.push("highpass=frequency=20:poles=2".to_owned());
    }

    // 2. Loudness compensation, labeled for in-place af-command updates.
    // Quantized to 1 dB: a full-slider drag crosses ~4 steps, so the chain
    // is rebuilt a few times per gesture at most, never per frame.
    {
        let (bass_db, treble_db) = loudness_compensation_db(volume_percent, amt.loudness);
        let bass_db = quantize_1db(bass_db);
        let treble_db = quantize_1db(treble_db);
        if bass_db >= 0.05 && caps.bass {
            parts.push(format!("@enh_loud_bass:bass=gain={}:frequency=120", fmt_db(bass_db)));
        }
        if treble_db >= 0.05 && caps.treble {
            parts
                .push(format!("@enh_loud_treble:treble=gain={}:frequency=9000", fmt_db(treble_db)));
        }
    }

    // 3. Tonal polish, max about +-2 dB at full amount.
    if amt.polish > 0.0 {
        let mud = -1.8 * amt.polish;
        let presence = 1.4 * amt.polish;
        let air = 1.6 * amt.polish;
        if caps.equalizer {
            if mud.abs() >= 0.05 {
                parts.push(format!(
                    "equalizer=frequency=320:width_type=q:width=1:gain={}",
                    fmt_db(mud)
                ));
            }
            if presence.abs() >= 0.05 {
                parts.push(format!(
                    "equalizer=frequency=3500:width_type=q:width=1:gain={}",
                    fmt_db(presence)
                ));
            }
        }
        if air.abs() >= 0.05 && caps.treble {
            parts.push(format!("treble=gain={}:frequency=10000", fmt_db(air)));
        }
    }

    // 4. Harmonic exciter, highs only, small blend.
    if amt.exciter > 0.0 && caps.aexciter {
        let amount = 0.3 * amt.exciter;
        parts.push(format!("aexciter=amount={:.2}:drive=6.0:blend=0:freq=6500:ceil=18000", amount));
    }

    // 5. Virtual bass, or a safe bass-shelf fallback.
    if amt.virtual_bass > 0.0 {
        if caps.virtualbass {
            let strength = 0.8 + 1.2 * amt.virtual_bass;
            parts.push(format!("virtualbass=cutoff=120:strength={strength:.2}"));
        } else if caps.bass {
            let g = 4.0 * amt.virtual_bass;
            if g >= 0.05 {
                parts.push(format!("bass=gain={}:frequency=80", fmt_db(g)));
            }
        }
    }

    // 6. Stereo image, output-dependent. Bass stays mono-compatible
    // (side-level only, no M/S split that touches low mids).
    if amt.stereo > 0.0 {
        match settings.output {
            EnhancerOutput::Headphones => {
                if caps.crossfeed {
                    // Explicit unity I/O: the defaults (level_in=0.9) cost
                    // ~1.7 dB on wide material (measured 2026-10-07). The end
                    // limiter owns clipping instead.
                    let s = 0.2 + 0.15 * amt.stereo;
                    parts.push(format!(
                        "crossfeed=strength={s:.2}:range=0.5:level_in=1.0:level_out=1.0"
                    ));
                }
                if caps.stereotools {
                    let slev = 1.0 + 0.15 * amt.stereo;
                    parts.push(format!("stereotools=slev={slev:.3}"));
                }
            }
            EnhancerOutput::Speakers => {
                if caps.stereotools {
                    let slev = 1.1 + 0.15 * amt.stereo;
                    parts.push(format!("stereotools=slev={slev:.3}"));
                }
            }
        }
    }

    // 7. Room: very subtle early reflections, off in Subtle (room == 0).
    // Unity gains: measured 2026-10-07 (ffmpeg ebur128) that in_gain=0.8 /
    // out_gain=0.9 attenuate the dry signal ~2.8 dB, which no trim should
    // have to buy back. Only the tiny decays add anything (<0.1 dB).
    if amt.room > 0.0 && caps.aecho {
        let base = 0.03 + 0.05 * amt.room;
        let d1 = base;
        let d2 = base * 0.8;
        let d3 = base * 0.6;
        parts.push(format!(
            "aecho=in_gain=1.0:out_gain=1.0:delays=12|24|32:decays={d1:.3}|{d2:.3}|{d3:.3}"
        ));
    }

    // 8. Output safety: gentle soft-clip into a -1 dBFS limiter with ASC.
    // `level=disabled` stops re-normalization (would undo the trim);
    // `latency=1` keeps gapless transitions seamless (see af_chain docs).
    if caps.asoftclip {
        parts.push("asoftclip=type=tanh:threshold=0.89:output=1".to_owned());
    }
    parts.push("alimiter=limit=0.89:level=disabled:asc=1:latency=1".to_owned());

    // Level-matching trim so the chain stays within ~0.5 dB of bypass.
    let trim = preset_trim_db(settings.preset);
    if trim.abs() >= 0.05 {
        parts.push(format!("volume={}dB", fmt_db(trim)));
    }

    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> EnhancerSettings {
        EnhancerSettings { enabled: true, ..EnhancerSettings::default() }
    }

    #[test]
    fn default_is_off_subtle() {
        let d = EnhancerSettings::default();
        assert!(!d.enabled);
        assert!(!d.bypass);
        assert_eq!(d.preset, EnhancerPresetName::Subtle);
        assert_eq!(d.output, EnhancerOutput::Speakers);
        assert_eq!(build_enhancer_af(&d, 80, &FilterCaps::all()), "");
    }

    #[test]
    fn bypass_returns_empty() {
        let mut s = on();
        s.bypass = true;
        assert_eq!(build_enhancer_af(&s, 50, &FilterCaps::all()), "");
    }

    #[test]
    fn subtle_has_limiter_and_no_room() {
        let mut s = on();
        s.preset = EnhancerPresetName::Subtle;
        s.amounts = EnhancerAmounts::from_preset(EnhancerPresetName::Subtle);
        let af = build_enhancer_af(&s, 80, &FilterCaps::all());
        assert!(af.contains("aformat=sample_fmts=fltp"), "{}", af);
        assert!(af.contains("highpass=frequency=20"), "{}", af);
        assert!(af.contains("alimiter=limit=0.89"), "{}", af);
        assert!(af.contains("asoftclip=type=tanh"), "{}", af);
        assert!(!af.contains("aecho"), "Subtle room is 0, must not smear: {}", af);
        assert!(af.contains("volume=-0.5dB"), "level trim missing: {}", af);
    }

    #[test]
    fn reference_is_transparent_plus_limiter() {
        let mut s = on();
        s.preset = EnhancerPresetName::Reference;
        s.amounts = EnhancerAmounts::from_preset(EnhancerPresetName::Reference);
        let af = build_enhancer_af(&s, 80, &FilterCaps::all());
        assert!(af.contains("alimiter"), "{}", af);
        assert!(!af.contains("aexciter"), "{}", af);
        assert!(!af.contains("virtualbass"), "{}", af);
        assert!(!af.contains("stereotools"), "{}", af);
        assert!(!af.contains("crossfeed"), "{}", af);
        assert!(!af.contains("aecho"), "{}", af);
    }

    #[test]
    fn loudness_grows_as_volume_drops() {
        let (b100, t100) = loudness_compensation_db(100, 1.0);
        let (b50, t50) = loudness_compensation_db(50, 1.0);
        let (b20, t20) = loudness_compensation_db(20, 1.0);
        assert_eq!((b100, t100), (0.0, 0.0));
        assert!(b50 > 0.5 && b50 < 4.0, "{}", b50);
        assert!(t50 > 0.3 && t50 < 3.0, "{}", t50);
        assert!(b20 > b50 && t20 > t50, "must grow as volume drops");
        assert!(b20 <= 6.01 && t20 <= 4.01, "capped: {}/{}", b20, t20);
        assert_eq!(loudness_compensation_db(70, 0.0), (0.0, 0.0));
    }

    #[test]
    fn headphones_adds_crossfeed_speakers_do_not() {
        let mut s = on();
        s.amounts = EnhancerAmounts {
            stereo: 1.0,
            ..EnhancerAmounts::from_preset(EnhancerPresetName::Wide)
        };
        s.output = EnhancerOutput::Headphones;
        let hp = build_enhancer_af(&s, 80, &FilterCaps::all());
        assert!(hp.contains("crossfeed=strength=0.35"), "{}", hp);
        s.output = EnhancerOutput::Speakers;
        let sp = build_enhancer_af(&s, 80, &FilterCaps::all());
        assert!(!sp.contains("crossfeed"), "{}", sp);
        assert!(sp.contains("stereotools=slev=1.250"), "{}", sp);
    }

    #[test]
    fn missing_filters_drop_stages_safely() {
        let s = on();
        // No exciter in this build: stage skipped, chain still valid.
        let mut caps = FilterCaps::all();
        caps.aexciter = false;
        let af = build_enhancer_af(&s, 80, &caps);
        assert!(!af.contains("aexciter"), "{}", af);
        assert!(af.contains("alimiter"), "{}", af);
        // No virtualbass: documented bass-shelf fallback.
        caps.virtualbass = false;
        let af2 = build_enhancer_af(&s, 80, &caps);
        assert!(!af2.contains("virtualbass"), "{}", af2);
        assert!(af2.contains("bass=gain="), "fallback missing: {}", af2);
        // No limiter anywhere: the whole chain is dropped, never clip.
        caps.alimiter = false;
        assert_eq!(build_enhancer_af(&s, 80, &caps), "");
        // Nothing at all: also empty, never an invalid "," chain.
        assert_eq!(build_enhancer_af(&s, 80, &FilterCaps::none()), "");
    }

    #[test]
    fn hostile_inputs_never_panic_or_emit_nan() {
        let caps = FilterCaps::all();
        for vol in [-100, 0, 50, 100, 1000] {
            for raw in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.5, 2.0] {
                let s = EnhancerSettings {
                    enabled: true,
                    bypass: false,
                    preset: EnhancerPresetName::Warm,
                    amounts: EnhancerAmounts {
                        loudness: raw,
                        polish: raw,
                        exciter: raw,
                        virtual_bass: raw,
                        stereo: raw,
                        room: raw,
                    },
                    output: EnhancerOutput::Headphones,
                };
                let af = build_enhancer_af(&s, vol, &caps);
                assert!(!af.contains("NaN"), "vol={} raw={}: {}", vol, raw, af);
                assert!(!af.contains("nan"), "vol={} raw={}: {}", vol, raw, af);
                assert!(!af.contains("inf"), "vol={} raw={}: {}", vol, raw, af);
                assert!(!af.contains(",,"), "vol={} raw={}: {}", vol, raw, af);
                assert!(!af.starts_with(',') && !af.ends_with(','), "{}", af);
            }
        }
    }

    #[test]
    fn loudness_is_quantized_to_1db_steps() {
        assert_eq!(quantize_1db(2.12), 2.0);
        assert_eq!(quantize_1db(2.5), 3.0);
        assert_eq!(quantize_1db(0.0), 0.0);
        assert_eq!(quantize_1db(f32::NAN), 0.0);
        assert_eq!(quantize_1db(f32::INFINITY), 0.0);
        // The builder only ever emits integer loudness gains: a drag that
        // stays inside one step rebuilds nothing.
        let mut s = on();
        s.amounts = EnhancerAmounts {
            loudness: 1.0,
            polish: 0.0,
            exciter: 0.0,
            virtual_bass: 0.0,
            stereo: 0.0,
            room: 0.0,
        };
        for vol in [0, 10, 20, 30, 40, 50, 60, 70, 80, 90] {
            let af = build_enhancer_af(&s, vol, &FilterCaps::all());
            for token in af.split(',') {
                if let Some(g) = token.split("gain=").nth(1) {
                    let db: f32 = g.split(':').next().unwrap_or("x").parse().unwrap();
                    assert_eq!(db, db.round(), "non-integer loudness gain at vol={vol}: {token}");
                }
            }
        }
        // Adjacent volumes share a step mid-slider (bass 2.1→2.1 dB):
        // no rebuild while dragging inside it.
        let a50 = build_enhancer_af(&s, 50, &FilterCaps::all());
        let a51 = build_enhancer_af(&s, 51, &FilterCaps::all());
        assert_eq!(a50, a51);
    }

    #[test]
    fn polish_stays_within_2db() {
        // The quality goal caps tonal moves at about +-2 dB at full amount.
        let mut s = on();
        s.amounts = EnhancerAmounts {
            polish: 1.0,
            loudness: 0.0,
            exciter: 0.0,
            virtual_bass: 0.0,
            stereo: 0.0,
            room: 0.0,
        };
        // Volume 100 so loudness comp contributes 0 and only polish shows.
        let af = build_enhancer_af(&s, 100, &FilterCaps::all());
        assert!(af.contains("gain=-1.8"), "{}", af);
        assert!(af.contains("gain=1.4"), "{}", af);
        assert!(af.contains("gain=1.6"), "{}", af);
    }
}
