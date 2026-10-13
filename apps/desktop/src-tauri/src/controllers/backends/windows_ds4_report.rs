//! Public ViGEm DS4 wire buffers and canonical report conversion.
use crate::controllers::mapping::XReport;

pub(super) fn header<const N: usize>(serial: u32) -> [u8; N] {
    let mut bytes = [0; N];
    if let Some(size) = bytes.get_mut(..4) {
        size.copy_from_slice(&u32::try_from(N).unwrap_or(u32::MAX).to_le_bytes());
    }
    if let Some(slot) = bytes.get_mut(4..8) {
        slot.copy_from_slice(&serial.to_le_bytes());
    }
    bytes
}
pub(super) fn plugin(serial: u32) -> [u8; 16] {
    let mut bytes = header::<16>(serial);
    bytes[8..12].copy_from_slice(&2_u32.to_le_bytes()); // DualShock4Wired
    bytes[12..14].copy_from_slice(&0x054c_u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&0x05c4_u16.to_le_bytes());
    bytes
}
fn axis(value: i16) -> u8 {
    u8::try_from(((i32::from(value) + 32768) * 255 + 32767) / 65535).unwrap_or_default()
}
pub(super) fn report(serial: u32, state: &XReport) -> [u8; 20] {
    let mut bytes = header::<20>(serial);
    bytes[8] = axis(state.left.0);
    bytes[9] = axis(state.left.1.saturating_neg());
    bytes[10] = axis(state.right.0);
    bytes[11] = axis(state.right.1.saturating_neg());
    let has = |bit| state.buttons & bit != 0;
    let vertical = i8::from(has(1)) - i8::from(has(2));
    let horizontal = i8::from(has(8)) - i8::from(has(4));
    let hat: u16 = match (horizontal, vertical) {
        (0, 1) => 0,
        (1, 1) => 1,
        (1, 0) => 2,
        (1, -1) => 3,
        (0, -1) => 4,
        (-1, -1) => 5,
        (-1, 0) => 6,
        (-1, 1) => 7,
        _ => 8,
    };
    let mut buttons = hat;
    for (source, target) in [
        (0x4000, 4),
        (0x1000, 5),
        (0x2000, 6),
        (0x8000, 7),
        (0x0100, 8),
        (0x0200, 9),
        (0x0020, 12),
        (0x0010, 13),
        (0x0040, 14),
        (0x0080, 15),
    ] {
        if has(source) {
            buttons |= 1 << target;
        }
    }
    if state.left_trigger > 0 {
        buttons |= 1 << 10;
    }
    if state.right_trigger > 0 {
        buttons |= 1 << 11;
    }
    bytes[12..14].copy_from_slice(&buttons.to_le_bytes());
    bytes[14] = u8::from(has(0x0400)); // PS
    bytes[15] = state.left_trigger;
    bytes[16] = state.right_trigger;
    bytes
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_bus_buffers_have_the_documented_sizes_offsets_and_zero_padding() {
        let plug = plugin(7);
        assert_eq!(
            plug,
            [16, 0, 0, 0, 7, 0, 0, 0, 2, 0, 0, 0, 0x4c, 5, 0xc4, 5]
        );
        let neutral = report(7, &XReport::default());
        assert_eq!(&neutral[..8], &[20, 0, 0, 0, 7, 0, 0, 0]);
        assert_eq!(&neutral[8..17], &[128, 128, 128, 128, 8, 0, 0, 0, 0]);
        assert_eq!(&neutral[17..], &[0, 0, 0]);
        assert_eq!(header::<16>(7)[8..], [0; 8]);
    }
    #[test]
    fn extremes_face_buttons_triggers_and_opposite_directions_map_correctly() {
        let state = XReport {
            left: (i16::MIN, i16::MIN),
            right: (i16::MAX, i16::MAX),
            buttons: 1 | 8 | 0x1000 | 0x400 | 0x20,
            left_trigger: 255,
            right_trigger: 1,
        };
        let bytes = report(1, &state);
        assert_eq!(&bytes[8..12], &[0, 255, 255, 0]);
        assert_eq!(
            u16::from_le_bytes([bytes[12], bytes[13]]),
            1 | (1 << 5) | (1 << 10) | (1 << 11) | (1 << 12)
        );
        assert_eq!(&bytes[14..17], &[1, 255, 1]);
        let opposite = report(
            1,
            &XReport {
                buttons: 15,
                ..XReport::default()
            },
        );
        assert_eq!(opposite[12] & 15, 8);
    }
}
