//! scrcpy-server v5.0.1 video stream framing (all integers big-endian).

pub const HEADER_LEN: usize = 12;
pub const DEVICE_NAME_LEN: usize = 64;
pub const CODEC_H264: u32 = u32::from_be_bytes(*b"h264");
/// Raw PCM s16le 48 kHz stereo (audio socket). Any other id means the
/// device can't capture audio (disabled/error) — video keeps going.
pub const CODEC_RAW: u32 = u32::from_be_bytes(*b"\0raw");

const FLAG_SESSION: u8 = 0x80;
const FLAG_CONFIG: u64 = 1 << 62;
const FLAG_KEY_FRAME: u64 = 1 << 61;
const PTS_MASK: u64 = FLAG_KEY_FRAME - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    /// New capture session (start, rotation, resize): video size.
    Session { width: u32, height: u32 },
    /// Media packet; `len` payload bytes follow. `pts` is None for config packets.
    Media {
        config: bool,
        key_frame: bool,
        pts: Option<u64>,
        len: u32,
    },
}

pub fn parse_header(h: &[u8; HEADER_LEN]) -> Header {
    let be32 = |i: usize| u32::from_be_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    if h[0] & FLAG_SESSION != 0 {
        return Header::Session {
            width: be32(4),
            height: be32(8),
        };
    }
    let pts_flags = u64::from_be_bytes(h[..8].try_into().unwrap());
    let config = pts_flags & FLAG_CONFIG != 0;
    Header::Media {
        config,
        key_frame: pts_flags & FLAG_KEY_FRAME != 0,
        pts: (!config).then_some(pts_flags & PTS_MASK),
        len: be32(8),
    }
}

/// NUL-padded UTF-8 device name field.
pub fn parse_device_name(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// `orientation=N` from the `Viewport INTERNAL` line of `dumpsys input`.
pub fn parse_orientation(dumpsys: &str) -> Option<u8> {
    let line = dumpsys.lines().find(|l| l.contains("Viewport INTERNAL"))?;
    let rest = line.split("orientation=").nth(1)?;
    rest.chars()
        .next()?
        .to_digit(10)
        .filter(|d| *d < 4)
        .map(|d| d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bytes captured from a vivo Pad3 running scrcpy-server 5.0.1.
    #[test]
    fn parses_session_packet() {
        let h = [0x80, 0, 0, 0, 0, 0, 0x05, 0x00, 0, 0, 0x03, 0x82];
        assert_eq!(
            parse_header(&h),
            Header::Session {
                width: 1280,
                height: 898
            }
        );
    }

    #[test]
    fn parses_config_packet() {
        let h = [0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x20];
        assert_eq!(
            parse_header(&h),
            Header::Media {
                config: true,
                key_frame: false,
                pts: None,
                len: 32
            }
        );
    }

    #[test]
    fn parses_key_frame_with_pts() {
        let h = [
            0x20, 0, 0, 0, 0xe1, 0x84, 0x1a, 0xfb, 0x00, 0x01, 0x18, 0xf3,
        ];
        assert_eq!(
            parse_header(&h),
            Header::Media {
                config: false,
                key_frame: true,
                pts: Some(0xe184_1afb),
                len: 71923
            }
        );
    }

    #[test]
    fn parses_regular_frame() {
        let h = [0x00, 0, 0, 0, 0xe1, 0x84, 0x41, 0x68, 0, 0, 0x14, 0x26];
        assert_eq!(
            parse_header(&h),
            Header::Media {
                config: false,
                key_frame: false,
                pts: Some(0xe184_4168),
                len: 5158
            }
        );
    }

    #[test]
    fn parses_device_name_and_codec() {
        let mut field = [0u8; DEVICE_NAME_LEN];
        field[..6].copy_from_slice(b"PA2455");
        assert_eq!(parse_device_name(&field), "PA2455");
        assert_eq!(CODEC_H264, 0x6832_3634);
        assert_eq!(CODEC_RAW, 0x0072_6177); // captured: 00 72 61 77
    }

    #[test]
    fn parses_orientation_from_dumpsys_input() {
        let out = "  Viewport INTERNAL: displayId=0, uniqueId=local:46, port=131, orientation=1, logicalFrame=[0, 0, 2800, 1968]";
        assert_eq!(parse_orientation(out), Some(1));
        assert_eq!(parse_orientation("nothing"), None);
    }
}
