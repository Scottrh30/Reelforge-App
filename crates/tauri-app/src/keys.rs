//! Keyframes for the web interface: the selected clip's keyframe buttons for
//! its transform, and the curve editor's channels (every transform component
//! and effect setting), with their keys and sampled curves.

use crate::{ed, Res};
use reelforge_core::{Channel, Clip, ClipId, Interp, Prop, Time, Transform};
use reelforge_engine::editor::Editor;
use serde::Serialize;

/// The transform properties with a keyframe button, by name.
const PROPS: [(Prop, &str); 9] = [
    (Prop::Position, "Position"),
    (Prop::Scale, "Scale"),
    (Prop::Rotation, "Rotation"),
    (Prop::Opacity, "Opacity"),
    (Prop::Anchor, "Anchor"),
    (Prop::Crop, "Crop"),
    (Prop::Pin, "Pin"),
    (Prop::Depth, "Depth"),
    (Prop::Orientation, "Orientation"),
];

/// How many points of each animated curve are sent.
const SAMPLES: usize = 240;

fn prop_named(s: &str) -> Option<Prop> {
    PROPS.iter().find(|(_, n)| *n == s).map(|(p, _)| *p)
}

/// "t:Position:0" (a transform property and component) or "e:2:1" (effect 2, setting 1).
fn channel_id(ch: Channel) -> String {
    match ch {
        Channel::Transform(p, c) => format!("t:{}:{c}", PROPS.iter().find(|(q, _)| *q == p).map_or("Pin", |(_, n)| n)),
        Channel::Effect(e, p) => format!("e:{e}:{p}"),
    }
}

fn parse_channel(s: &str) -> Option<Channel> {
    let mut it = s.split(':');
    match (it.next()?, it.next()?, it.next()?) {
        ("t", p, c) => Some(Channel::Transform(prop_named(p)?, c.parse().ok()?)),
        ("e", e, p) => Some(Channel::Effect(e.parse().ok()?, p.parse().ok()?)),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct KeysView {
    /// The playhead is on the clip (keys can be set).
    pub on_clip: bool,
    /// The transform as seen at the playhead (keyframes applied).
    pub shown: Transform,
    pub props: Vec<PropKeys>,
    pub channels: Vec<ChannelView>,
    /// The time span the curve editor shows (the clip, a little either side), timeline ticks.
    pub span: (i64, i64),
    /// The transform keys just before and after the playhead (timeline ticks).
    pub prev_key: Option<i64>,
    pub next_key: Option<i64>,
    /// How the transform keys at the playhead move on (when there are some there).
    pub interp_here: Option<Interp>,
    /// The corner pin is on but its corners cross over.
    pub pin_twisted: bool,
}

#[derive(Serialize)]
pub struct PropKeys {
    pub prop: &'static str,
    pub animated: bool,
    pub key_here: bool,
    pub count: usize,
}

#[derive(Serialize)]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    /// "Transform", or the effect's name.
    pub group: String,
    pub unit: &'static str,
    /// Allowed range (display units), if it has one.
    pub range: Option<(f32, f32)>,
    /// The value at the playhead (display units).
    pub value: f32,
    pub keys: Vec<KeyView>,
    /// The curve over `span`, `SAMPLES` values (display units); empty when not animated.
    pub samples: Vec<f32>,
}

#[derive(Serialize)]
pub struct KeyView {
    /// Timeline time.
    pub at: i64,
    pub value: f32,
    pub interp: Interp,
}

/// Where a key at clip (source) time `at` is on the timeline.
fn timeline_of(c: &Clip, at: Time) -> Time {
    c.timeline_start + (at - c.source_in)
}

pub fn view(e: &Editor, c: &Clip) -> KeysView {
    let v = c.visual();
    let (src, on_clip) = e.key_time(c);
    let shown = v.anim.eval(&v.transform, src);
    let rate = e.seq().frame_rate;
    let pad = Time((c.duration.0 / 20).max(rate.frame_duration().0));
    let (t0, t1) = (c.timeline_start - pad, c.timeline_end() + pad);
    let ph = e.playback.playhead;
    let times: Vec<Time> = v.anim.key_times().into_iter().map(|t| timeline_of(c, t)).collect();
    let channels = v
        .channels()
        .into_iter()
        .filter(|ch| !matches!(ch, Channel::Transform(Prop::Pin, _)))
        .map(|ch| {
            let (mul, unit) = v.channel_display(ch);
            let keys: Vec<KeyView> = v.channel_keys(ch).into_iter().map(|(at, val, interp)| KeyView { at: timeline_of(c, at).0, value: val * mul, interp }).collect();
            let samples = if keys.is_empty() {
                Vec::new()
            } else {
                (0..SAMPLES)
                    .map(|i| {
                        let t = Time(t0.0 + ((t1.0 - t0.0) as f64 * i as f64 / (SAMPLES - 1) as f64) as i64);
                        v.channel_value(ch, c.source_time(t.max(c.timeline_start).min(c.timeline_end()))) * mul
                    })
                    .collect()
            };
            let group = match ch {
                Channel::Transform(..) => "Transform".to_string(),
                Channel::Effect(i, _) => v.effects.get(i).map(|f| f.label()).unwrap_or_default(),
            };
            let name = match ch {
                Channel::Effect(i, p) => v.effects.get(i).and_then(|f| f.spec(p)).map_or("?".to_string(), |s| s.name.to_string()),
                _ => v.channel_name(ch),
            };
            ChannelView {
                id: channel_id(ch),
                name,
                group,
                unit,
                range: v.channel_range(ch).map(|(lo, hi)| (lo * mul, hi * mul)),
                value: v.channel_value(ch, src) * mul,
                keys,
                samples,
            }
        })
        .collect();
    KeysView {
        on_clip,
        shown,
        props: PROPS
            .iter()
            .map(|(p, name)| PropKeys { prop: name, animated: v.anim.prop_animated(*p), key_here: on_clip && v.anim.has_key(*p, src), count: v.anim.key_count(*p) })
            .collect(),
        channels,
        span: (t0.0, t1.0),
        prev_key: times.iter().rev().copied().find(|t| *t < ph).map(|t| t.0),
        next_key: times.iter().copied().find(|t| *t > ph).map(|t| t.0),
        interp_here: v.anim.interp_at(src).filter(|_| on_clip),
        pin_twisted: shown.pin.is_some() && shown.valid_pin().is_none(),
    }
}

// ---- Commands ----------------------------------------------------------------------

/// One-click transform changes, each one undo step: "anchor_centre",
/// "crop_clear", "pin_on", "pin_off", "pin_reset", "fit", "fill", "reset".
#[tauri::command]
pub fn transform_op(id: u64, op: String) -> Res<()> {
    ed(|e| {
        let id = ClipId(id);
        e.commit_live_edit();
        let Some((_, c)) = e.seq().find_clip(id) else { return };
        let native = e.layer_size(c);
        let frame = e.frame_px();
        let (src, _) = e.key_time(c);
        let Some(mut v) = e.visual(id) else { return };
        let mut t = v.anim.eval(&v.transform, src);
        match op.as_str() {
            "anchor_centre" => t = t.with_anchor_kept_in_place([0.0, 0.0], frame),
            "crop_clear" => t.crop = [0.0; 4],
            "pin_on" | "pin_reset" => t.pin = Some(t.pin_of_transform(native, frame)),
            "fit" | "fill" => {
                let k = Transform::fit_scale(native, frame, op == "fill");
                t.scale = [k, k];
                t.position = [0.0, 0.0];
            }
            "pin_off" | "reset" => {
                if op == "reset" {
                    v.transform = Transform::default();
                    v.anim = Default::default();
                } else {
                    v.transform.pin = None;
                    v.anim.pin.0.clear();
                }
                e.set_visual_live(id, v);
                e.commit_live_edit();
                return;
            }
            _ => return,
        }
        e.set_transform_live(id, t);
        e.commit_live_edit();
    })
}

/// How the transform keys at the playhead move towards the next ones.
#[tauri::command]
pub fn transform_interp(id: u64, interp: Interp) -> Res<()> {
    ed(|e| {
        let id = ClipId(id);
        e.commit_live_edit();
        let Some((_, c)) = e.seq().find_clip(id) else { return };
        let (src, on_clip) = e.key_time(c);
        let Some(mut v) = e.visual(id) else { return };
        if on_clip {
            v.anim.set_interp_at(src, interp);
            e.set_visual_live(id, v);
            e.commit_live_edit();
        }
    })
}

/// The keyframe button of a transform property ("Position", "Scale", …).
#[tauri::command]
pub fn transform_key(id: u64, prop: String) -> Res<()> {
    let p = prop_named(&prop).ok_or("unknown property")?;
    ed(|e| e.toggle_transform_key(ClipId(id), p))
}

/// Drag key `index` of `channel` to timeline time `at` and `value` (display
/// units): live, `commit` ends it. `keep_time` moves only the value.
/// Returns the timeline time actually used.
#[tauri::command]
pub fn key_move(id: u64, channel: String, index: usize, at: i64, value: f32, keep_time: bool) -> Res<Option<i64>> {
    let ch = parse_channel(&channel).ok_or("unknown channel")?;
    ed(|e| {
        let id = ClipId(id);
        let rate = e.seq().frame_rate;
        let (_, c) = e.seq().find_clip(id)?;
        let c = c.clone();
        let mut v = e.visual(id)?;
        let (mul, _) = v.channel_display(ch);
        let (orig_at, ..) = *v.channel_keys(ch).get(index)?;
        let src = if keep_time { orig_at } else { c.source_time(Time(at).snap(rate).max(c.timeline_start).min(c.timeline_end())) };
        let src = src.max(c.source_in).min(c.source_in + c.duration);
        let used = v.channel_set_key(ch, index, src, value / mul, rate.frame_duration());
        e.set_visual_live(id, v);
        Some(timeline_of(&c, used).0)
    })
}

/// A key's interpolation: "Linear", "Ease" or "Hold". One undo step.
#[tauri::command]
pub fn key_interp(id: u64, channel: String, index: usize, interp: Interp) -> Res<()> {
    let ch = parse_channel(&channel).ok_or("unknown channel")?;
    one_step(id, |v| v.channel_set_interp(ch, index, interp))
}

#[tauri::command]
pub fn key_delete(id: u64, channel: String, index: usize) -> Res<()> {
    let ch = parse_channel(&channel).ok_or("unknown channel")?;
    one_step(id, |v| v.channel_remove_key(ch, index))
}

/// Add a key on `channel` at timeline time `at` (holding the curve's value
/// there). Returns its index.
#[tauri::command]
pub fn key_add(id: u64, channel: String, at: i64) -> Res<Option<usize>> {
    let ch = parse_channel(&channel).ok_or("unknown channel")?;
    ed(|e| {
        e.commit_live_edit();
        let cid = ClipId(id);
        let rate = e.seq().frame_rate;
        let (_, c) = e.seq().find_clip(cid)?;
        let src = c.source_time(Time(at).snap(rate).max(c.timeline_start).min(c.timeline_end()));
        let src = src.max(c.source_in).min(c.source_in + c.duration);
        let mut v = e.visual(cid)?;
        let k = v.channel_add_key(ch, src);
        e.set_visual_live(cid, v);
        e.commit_live_edit();
        k
    })
}

fn one_step(id: u64, f: impl FnOnce(&mut reelforge_core::ClipVisual)) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        let cid = ClipId(id);
        if let Some(mut v) = e.visual(cid) {
            f(&mut v);
            e.set_visual_live(cid, v);
            e.commit_live_edit();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_ids_round_trip() {
        for ch in [Channel::Transform(Prop::Position, 1), Channel::Transform(Prop::Crop, 3), Channel::Transform(Prop::Opacity, 0), Channel::Effect(2, 5)] {
            assert_eq!(parse_channel(&channel_id(ch)), Some(ch));
        }
        assert_eq!(parse_channel("t:Nope:0"), None);
        assert_eq!(parse_channel("x"), None);
    }
}
