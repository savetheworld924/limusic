# Psychoacoustic Enhancer

Optional mpv `af` chain that makes music sound more detailed, wider and more
"in front of you" without changing the masters. Default **OFF**. No PCM
reroute, no new audio device, no heavy deps: gapless, 2-deck crossfade and
`normalize_volume` keep working.

## Chain order

`aformat(fltp)` → `highpass 20 Hz` → loudness comp → tonal polish → exciter →
virtual bass → stereo → room → `asoftclip` → `alimiter(-1 dBFS)` → trim.

0. **aformat `sample_fmts=fltp`** — float precision for every stage.
1. **highpass 20 Hz** — subsonic rumble out, inaudible otherwise.
2. **Loudness compensation** — `bass(f=120)` + `treble(f=9000)` whose gain
   grows as the app volume drops (`t^1.5`, bass ≤6 dB, treble ≤4 dB, scaled by
   the Loudness slider). Re-applied on volume change, throttled to ~10/s.
   Labeled `@enh_loud_bass/@enh_loud_treble` for future `af-command` updates.
3. **Tonal polish** — mud cut ~320 Hz (Q), presence ~3.5 kHz (Q), air shelf
   ~10 kHz. Capped at about ±2 dB at full slider so it never gets harsh.
4. **Harmonic exciter** — `aexciter(freq=6500, ceil=18000, small amount)`.
   Adds highs that EQ alone cannot, for "crisp" without brightness.
5. **Virtual bass** — `virtualbass(cutoff=120)` adds implied low end for
   small speakers/headphones. Fallback when missing: gentle `bass(f=80)`
   shelf. No low-end boost that distorts.
6. **Stereo** — depends on Listening-on:
   - headphones: `crossfeed(strength 0.2–0.35)` + tiny `stereotools(slev)` widening;
   - speakers: `stereotools(slev 1.1–1.25)` only, no crossfeed.
   Side-level only, so bass stays mono-compatible.
7. **Room** — `aecho` with 12/24/32 ms taps, decays 0.03–0.08. Off in Subtle.
   Must stay subliminal: if you hear echo, turn it down.
8. **Safety** — `asoftclip(tanh)` into `alimiter(limit=0.89 ≈ −1 dBFS,
   asc=1, level=disabled, latency=1)`. Never clips. `latency=1` keeps gapless
   seamless (same reason as the normalize limiter).

## Level-matched A/B

The chain carries a per-preset trim (`volume=-XdB`: Subtle −0.4, Warm −0.8,
Wide −0.6, Reference −0.2) so enhanced lands within ~0.5 dB of bypass.
Bypass rebuilds `af` to the plain gain+pitch chain (same path as the
normalize toggle), so there is no loudness jump and no click in practice.
If mpv refuses the full chain, the enhancer disables itself and restores
gain+pitch — never silence, never crash.

Calibrate with ffmpeg: render pink noise + a music clip + a loud master
through the chain vs bypass, compare integrated loudness with `ebur128` or
`astats`, adjust `preset_trim_db()` in `crates/player/src/enhancer.rs`.

## Presets

- **Subtle** (default): loudness 0.6, polish 0.4, exciter 0.25, bass 0.3,
  stereo 0.4, room 0. The everyday one.
- **Warm**: more virtual bass + room, less exciter. Small speakers.
- **Wide**: more polish/exciter/stereo. Headphones, already-wide mixes beware.
- **Reference**: loudness comp + limiter only. Almost transparent.

Each stage has its own 0–100% slider (JSON blob `enhancer_amounts`); picking
a preset rewrites all six to that preset.

## Tune it

1. Start Subtle on speakers, A/B on a familiar chorus. You should hear a
   small but obvious lift, no level jump.
2. Too bright? Lower Exciter before Polish.
3. Too boomy? Lower Virtual bass before Loudness.
4. Image too wide? Lower Stereo; on headphones try Speakers mode (no crossfeed).
5. Hear smear/echo? Room is too high — it should be felt, not heard.
6. Volume changes re-tune loudness comp automatically; nothing else to do.

## Compatibility

At startup each filter is probed via a minimal `af` set; a missing filter
drops its stage (virtualbass → bass-shelf fallback) and logs it. No
`alimiter` anywhere disables the whole enhancer (cannot guarantee no-clip).
Low CPU by design: a handful of biquads + stock ffmpeg filters, fine on a
weak laptop.

## Files

- `crates/player/src/enhancer.rs` — chain builder, presets, trims, caps, tests.
- `crates/player/src/lib.rs` — `Player::{enhancer,set_enhancer,
  set_enhancer_bypass,probe_enhancer_filters,apply_af,full_af_chain}`,
  volume-throttled re-apply.
- `src-tauri/src/commands.rs` — 5 `UI_SETTINGS` keys + immediate apply.
- `src-tauri/src/state.rs` — `load_enhancer_settings`,
  `parse_enhancer_amounts`, `enhancer_amounts_json`, `apply_enhancer`.
- `src-tauri/src/lib.rs` — startup apply + `enhancer_bypass=false` reset.
- `ui/src/lib/components/EnhancerPanel.svelte` — panel (toggle, preset,
  output, 6 sliders, A/B). Wired into `SettingsDialog.svelte` playback tab.
