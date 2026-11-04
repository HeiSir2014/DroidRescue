//! Device audio playback: raw PCM s16le 48 kHz stereo from scrcpy-server →
//! WASAPI (cpal). Low latency by design: a small jitter buffer that drops
//! the oldest audio when it grows past a cap, and re-prebuffers on underrun.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Source is always stereo s16le at this rate.
pub const SOURCE_RATE: u32 = 48_000;
/// Jitter buffer: start playing at TARGET, trim back to TARGET above CAP.
pub const TARGET_MS: u32 = 40;
/// Target + two 21 ms device packets: absorbs USB jitter, but clock drift
/// can never let latency creep far above the target.
pub const CAP_MS: u32 = 80;

const fn frames_for_ms(ms: u32) -> usize {
    (SOURCE_RATE * ms / 1000) as usize
}

/// Interleaved s16le bytes → stereo frames in [-1, 1].
pub fn s16le_to_frames(bytes: &[u8]) -> impl Iterator<Item = [f32; 2]> + '_ {
    bytes.chunks_exact(4).map(|c| {
        let l = i16::from_le_bytes([c[0], c[1]]);
        let r = i16::from_le_bytes([c[2], c[3]]);
        [f32::from(l) / 32768.0, f32::from(r) / 32768.0]
    })
}

/// How many of the oldest frames to drop so latency stays bounded.
pub fn frames_to_drop(buffered: usize, target: usize, cap: usize) -> usize {
    if buffered > cap { buffered - target } else { 0 }
}

/// Jitter buffer + linear resampler, shared between the network thread
/// (producer) and the audio callback (consumer).
pub struct Buffer {
    frames: VecDeque<[f32; 2]>,
    target: usize,
    cap: usize,
    /// Waiting for TARGET frames before (re)starting playback.
    prebuffering: bool,
    /// Fractional read position between frames[0] and frames[1].
    phase: f64,
    /// Source frames consumed per output frame.
    step: f64,
}

impl Buffer {
    pub fn new(output_rate: u32) -> Self {
        Self {
            frames: VecDeque::new(),
            target: frames_for_ms(TARGET_MS),
            cap: frames_for_ms(CAP_MS),
            prebuffering: true,
            phase: 0.0,
            step: f64::from(SOURCE_RATE) / f64::from(output_rate),
        }
    }

    pub fn push(&mut self, frames: impl Iterator<Item = [f32; 2]>) {
        self.frames.extend(frames);
        let drop = frames_to_drop(self.frames.len(), self.target, self.cap);
        self.frames.drain(..drop);
        if self.prebuffering && self.frames.len() >= self.target {
            self.prebuffering = false;
        }
    }

    pub fn buffered_ms(&self) -> f32 {
        self.frames.len() as f32 * 1000.0 / SOURCE_RATE as f32
    }

    /// Fill an interleaved output buffer with `channels` channels.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        for frame in out.chunks_mut(channels) {
            let sample = if self.prebuffering || self.frames.len() < 2 {
                self.prebuffering = true; // underrun: wait for TARGET again
                [0.0, 0.0]
            } else {
                let (a, b) = (self.frames[0], self.frames[1]);
                let t = self.phase as f32;
                let s = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                self.phase += self.step;
                while self.phase >= 1.0 && self.frames.len() >= 2 {
                    self.frames.pop_front();
                    self.phase -= 1.0;
                }
                s
            };
            match channels {
                1 => frame[0] = (sample[0] + sample[1]) * 0.5,
                _ => {
                    frame[0] = sample[0];
                    frame[1] = sample[1];
                    for extra in &mut frame[2..] {
                        *extra = 0.0;
                    }
                }
            }
        }
    }
}

/// Output stream on the default device. Not `Send` on every platform, so the
/// thread that creates it must own it.
pub struct Player {
    _stream: cpal::Stream,
    pub buffer: Arc<Mutex<Buffer>>,
    pub description: String,
}

impl Player {
    pub fn open() -> Result<Player, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("没有可用的音频输出设备")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("读取音频输出格式失败: {e}"))?;
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!(
                "不支持的输出采样格式 {:?}",
                supported.sample_format()
            ));
        }
        let config: cpal::StreamConfig = supported.config();
        let channels = usize::from(config.channels);
        let rate = config.sample_rate;
        let buffer = Arc::new(Mutex::new(Buffer::new(rate)));
        let cb_buffer = buffer.clone();
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], _| cb_buffer.lock().unwrap().render(out, channels),
                |_err| {},
                Some(Duration::from_secs(2)),
            )
            .map_err(|e| format!("打开音频输出失败: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("启动音频播放失败: {e}"))?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "默认设备".into());
        Ok(Player {
            _stream: stream,
            buffer,
            description: format!("{name} · {rate} Hz · {channels}ch"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_s16le_stereo() {
        let bytes = [0x00, 0x40, 0x00, 0xC0, 0xFF, 0x7F, 0x00, 0x80];
        let frames: Vec<_> = s16le_to_frames(&bytes).collect();
        assert_eq!(frames, vec![[0.5, -0.5], [32767.0 / 32768.0, -1.0]]);
    }

    #[test]
    fn latency_policy_trims_back_to_target_only_above_cap() {
        assert_eq!(frames_to_drop(1000, 1920, 5760), 0);
        assert_eq!(frames_to_drop(5760, 1920, 5760), 0);
        assert_eq!(frames_to_drop(6000, 1920, 5760), 4080);
    }

    #[test]
    fn prebuffers_until_target_then_plays() {
        let mut b = Buffer::new(48_000);
        b.push(std::iter::repeat_n([0.25, 0.25], 100));
        let mut out = [1.0f32; 8];
        b.render(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0), "silent while prebuffering");

        b.push(std::iter::repeat_n([0.25, 0.25], frames_for_ms(TARGET_MS)));
        b.render(&mut out, 2);
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn buffer_never_exceeds_cap() {
        let mut b = Buffer::new(48_000);
        b.push(std::iter::repeat_n([0.0, 0.0], frames_for_ms(500)));
        assert!(b.buffered_ms() <= TARGET_MS as f32 + 0.1);
    }

    #[test]
    fn resamples_48k_to_96k_by_interpolating() {
        let mut b = Buffer::new(96_000);
        let ramp = (0..frames_for_ms(TARGET_MS)).map(|i| [i as f32, i as f32]);
        b.push(ramp);
        let mut out = [0.0f32; 8]; // 4 stereo output frames
        b.render(&mut out, 2);
        assert_eq!(out, [0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 1.5, 1.5]);
    }

    #[test]
    fn downmixes_to_mono_and_pads_extra_channels() {
        let mut b = Buffer::new(48_000);
        b.push(std::iter::repeat_n([1.0, 0.0], frames_for_ms(TARGET_MS)));
        let mut mono = [9.0f32; 2];
        b.render(&mut mono, 1);
        assert_eq!(mono, [0.5, 0.5]);
        let mut quad = [9.0f32; 4];
        b.render(&mut quad, 4);
        assert_eq!(quad, [1.0, 0.0, 0.0, 0.0]);
    }
}
