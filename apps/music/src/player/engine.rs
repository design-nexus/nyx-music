//! GStreamer playback: a `playbin` whose audio passes through
//! `equalizer-10bands ! volume (ReplayGain + preamp) ! spectrum`.
//! The bus is watched on the GTK main loop, so every callback runs on the UI thread.

use gst::prelude::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Spectrum resolution: 1024 bands from 0 Hz to Nyquist.
pub const BANDS: u32 = 1024;
/// One spectrum frame every 33 ms (≈30 per second); the widget smooths between them.
const INTERVAL_NS: u64 = 33_333_333;
pub const THRESHOLD_DB: i32 = -80;

/// What the faders are labelled.
pub const EQ_LABELS: [f64; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

pub struct Frame {
    /// Pipeline running time at which this frame's audio is heard.
    pub end_ns: u64,
    pub magnitudes: Vec<f32>,
    pub rate: u32,
}

pub struct Engine {
    /// Tells this engine's bus messages apart from the other one's (crossfade).
    pub id: u8,
    pub playbin: gst::Element,
    eq: Option<gst::Element>,
    gain: Option<gst::Element>,
    spectrum: Option<gst::Element>,
    pub has_spectrum: bool,
    /// The URI `about-to-finish` should switch to (gapless).
    next_uri: Arc<Mutex<Option<String>>>,
    /// Set when `about-to-finish` switched songs; the next STREAM_START is that switch.
    switched: Arc<AtomicBool>,
    frames: VecDeque<Frame>,
    rate: u32,
    _watch: Option<gst::bus::BusWatchGuard>,
}

/// Which optional GStreamer elements are missing (named by package).
pub fn missing() -> Vec<(&'static str, &'static str)> {
    let mut v = Vec::new();
    for (el, what) in [("spectrum", "spectrum analyzer"), ("equalizer-10bands", "equalizer")] {
        if gst::ElementFactory::find(el).is_none() {
            v.push((el, what));
        }
    }
    v
}

/// An audio output: PipeWire's node name (what the settings keep) and what to call it.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub id: String,
    pub name: String,
    pub default: bool,
}

/// The audio outputs there are now.
pub fn outputs() -> Vec<Output> {
    if gst::init().is_err() {
        return Vec::new();
    }
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Sink"), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let found = monitor
        .devices()
        .iter()
        .filter_map(|d| {
            let props = d.properties()?;
            let id = props.get::<String>("node.name").ok()?;
            Some(Output { id, name: d.display_name().to_string(), default: props.get::<bool>("is-default").unwrap_or(false) })
        })
        .collect();
    monitor.stop();
    found
}

/// A sink that plays through output `id` (PipeWire falls back to the default when
/// it's gone); `None` without PipeWire's GStreamer plugin.
fn sink_for(id: &str) -> Option<gst::Element> {
    let sink = gst::ElementFactory::make("pipewiresink").build().ok()?;
    sink.set_property("target-object", id);
    Some(sink)
}

fn make(name: &str) -> Option<gst::Element> {
    gst::ElementFactory::make(name).build().ok()
}

impl Engine {
    pub fn new() -> anyhow::Result<Engine> {
        gst::init()?;
        let playbin = gst::ElementFactory::make("playbin").name("music").build()?;
        // Audio only: no video window, no subtitles.
        playbin.set_property_from_str("flags", "audio+soft-volume");
        // Developer aid: MUSIC_AUDIO_SINK=fakesink plays silently (in real time) for tests.
        if let Some(name) = std::env::var("MUSIC_AUDIO_SINK").ok().filter(|s| !s.is_empty())
            && let Ok(sink) = gst::ElementFactory::make(&name).build()
        {
            if sink.has_property("sync") {
                sink.set_property("sync", true);
            }
            playbin.set_property("audio-sink", &sink);
        } else if let Some(sink) = Some(crate::prefs::get().output).filter(|o| !o.is_empty()).and_then(|o| sink_for(&o)) {
            playbin.set_property("audio-sink", &sink);
        }

        let bin = gst::Bin::with_name("music-filter");
        let convert_in = make("audioconvert");
        let eq = make("equalizer-10bands");
        let gain = make("volume");
        let convert_out = make("audioconvert");
        let spectrum = make("spectrum");
        let has_spectrum = spectrum.is_some();
        if let Some(s) = &spectrum {
            s.set_property("bands", BANDS);
            s.set_property("interval", INTERVAL_NS);
            s.set_property("threshold", THRESHOLD_DB);
            s.set_property("post-messages", true);
            s.set_property("message-phase", false);
        }
        let chain: Vec<gst::Element> =
            [convert_in, eq.clone(), gain.clone(), convert_out, spectrum.clone()].into_iter().flatten().collect();
        if let (Some(first), Some(last)) = (chain.first(), chain.last()) {
            bin.add_many(&chain)?;
            gst::Element::link_many(&chain)?;
            let sink = gst::GhostPad::with_target(&first.static_pad("sink").expect("sink pad"))?;
            let src = gst::GhostPad::with_target(&last.static_pad("src").expect("src pad"))?;
            bin.add_pad(&sink)?;
            bin.add_pad(&src)?;
            playbin.set_property("audio-filter", &bin);
        }

        let next_uri: Arc<Mutex<Option<String>>> = Arc::default();
        let switched = Arc::new(AtomicBool::new(false));
        {
            let next_uri = next_uri.clone();
            let switched = switched.clone();
            // Called on a streaming thread shortly before the current song ends.
            playbin.connect("about-to-finish", false, move |args| {
                let pb = args[0].get::<gst::Element>().ok()?;
                if let Some(uri) = next_uri.lock().ok()?.take() {
                    switched.store(true, Ordering::SeqCst);
                    pb.set_property("uri", uri);
                }
                None
            });
        }

        Ok(Engine {
            id: 0,
            playbin,
            eq,
            gain,
            spectrum,
            has_spectrum,
            next_uri,
            switched,
            frames: VecDeque::new(),
            rate: 44100,
            _watch: None,
        })
    }

    pub fn watch(&mut self, f: impl Fn(&gst::Message) + 'static) {
        if let Some(bus) = self.playbin.bus() {
            self._watch = bus
                .add_watch_local(move |_, msg| {
                    f(msg);
                    gst::glib::ControlFlow::Continue
                })
                .ok();
        }
    }

    pub fn has_eq(&self) -> bool {
        self.eq.is_some()
    }

    /// Play through output `id` ("" for the system's default). Leaves it stopped
    /// (Ready): the caller plays again and seeks back.
    pub fn set_output(&mut self, id: &str) {
        let _ = self.playbin.set_state(gst::State::Ready);
        self.frames.clear();
        let sink = if id.is_empty() { None } else { sink_for(id) };
        self.playbin.set_property("audio-sink", sink.as_ref());
    }

    /// Load a song (stopped first, so it starts from the top).
    pub fn load(&mut self, uri: &str) {
        let _ = self.playbin.set_state(gst::State::Ready);
        self.switched.store(false, Ordering::SeqCst);
        self.set_next(None);
        self.frames.clear();
        self.playbin.set_property("uri", uri);
    }

    pub fn set_next(&self, uri: Option<String>) {
        if let Ok(mut n) = self.next_uri.lock() {
            *n = uri;
        }
    }

    /// True once per gapless switch.
    pub fn take_switched(&self) -> bool {
        self.switched.swap(false, Ordering::SeqCst)
    }

    pub fn play(&self) {
        let _ = self.playbin.set_state(gst::State::Playing);
    }

    pub fn pause(&self) {
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    pub fn stop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
        self.frames.clear();
        self.set_next(None);
    }

    pub fn position(&self) -> Option<f64> {
        self.playbin.query_position::<gst::ClockTime>().map(|t| t.nseconds() as f64 / 1e9)
    }

    pub fn duration(&self) -> Option<f64> {
        self.playbin.query_duration::<gst::ClockTime>().map(|t| t.nseconds() as f64 / 1e9).filter(|d| *d > 0.0)
    }

    pub fn seek(&mut self, secs: f64) {
        self.frames.clear();
        let t = gst::ClockTime::from_nseconds((secs.max(0.0) * 1e9) as u64);
        let _ = self.playbin.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, t);
    }

    /// Linear volume, 0..1.
    pub fn set_volume(&self, linear: f64, muted: bool) {
        self.playbin.set_property("volume", linear.clamp(0.0, 1.0));
        self.playbin.set_property("mute", muted);
    }

    pub fn set_eq(&self, bands: &[f64; 10]) {
        if let Some(eq) = &self.eq {
            for (i, v) in bands.iter().enumerate() {
                eq.set_property(&format!("band{i}"), v.clamp(-24.0, 12.0));
            }
        }
    }

    /// ReplayGain × preamp, as one linear factor.
    pub fn set_gain(&self, factor: f64) {
        if let Some(g) = &self.gain {
            g.set_property("volume", factor.clamp(0.0, 10.0));
        }
    }

    /// Keep a spectrum frame until its audio is heard.
    pub fn push_spectrum(&mut self, s: &gst::StructureRef) {
        let Ok(list) = s.get::<gst::List>("magnitude") else { return };
        let magnitudes: Vec<f32> = list.iter().filter_map(|v| v.get::<f32>().ok()).collect();
        let running = s.get::<u64>("running-time").unwrap_or(0);
        let duration = s.get::<u64>("duration").unwrap_or(INTERVAL_NS);
        if let Some(rate) = self.sample_rate() {
            self.rate = rate;
        }
        self.frames.push_back(Frame { end_ns: running + duration, magnitudes, rate: self.rate });
        // Never let a stalled clock pile frames up.
        while self.frames.len() > 64 {
            self.frames.pop_front();
        }
    }

    fn sample_rate(&self) -> Option<u32> {
        let caps = self.spectrum.as_ref()?.static_pad("sink")?.current_caps()?;
        caps.structure(0)?.get::<i32>("rate").ok().map(|r| r as u32)
    }

    /// The latest frame whose audio is playing now.
    pub fn current_frame(&mut self) -> Option<&Frame> {
        let now = self.running_time()?;
        while self.frames.len() > 1 && self.frames.get(1).is_some_and(|f| f.end_ns <= now) {
            self.frames.pop_front();
        }
        self.frames.front().filter(|f| f.end_ns <= now + INTERVAL_NS)
    }

    fn running_time(&self) -> Option<u64> {
        let clock = self.playbin.clock()?;
        let base = self.playbin.base_time()?;
        Some(clock.time().nseconds().saturating_sub(base.nseconds()))
    }
}
