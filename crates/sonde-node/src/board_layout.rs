// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use sonde_protocol::BoardLayout;

#[cfg(feature = "esp")]
const RTC_LAYOUT_MAGIC: u32 = 0x534C_5954;

#[cfg(feature = "esp")]
#[repr(C)]
struct RtcBoardLayout {
    magic: u32,
    i2c0_sda: i16,
    i2c0_scl: i16,
    one_wire_data: i16,
    battery_adc: i16,
    sensor_enable: i16,
}

#[cfg(feature = "esp")]
#[link_section = ".rtc.data"]
static mut RTC_BOARD_LAYOUT: RtcBoardLayout = RtcBoardLayout {
    magic: 0,
    i2c0_sda: -1,
    i2c0_scl: -1,
    one_wire_data: -1,
    battery_adc: -1,
    sensor_enable: -1,
};

#[cfg(feature = "esp")]
const fn encode_pin(pin: Option<u8>) -> i16 {
    match pin {
        Some(pin) => pin as i16,
        None => -1,
    }
}

#[cfg(feature = "esp")]
const fn decode_pin(pin: i16) -> Option<u8> {
    if pin < 0 {
        None
    } else {
        Some(pin as u8)
    }
}

#[cfg(feature = "esp")]
pub fn stage_runtime_board_layout(layout: &BoardLayout) {
    unsafe {
        RTC_BOARD_LAYOUT.magic = RTC_LAYOUT_MAGIC;
        RTC_BOARD_LAYOUT.i2c0_sda = encode_pin(layout.i2c0_sda);
        RTC_BOARD_LAYOUT.i2c0_scl = encode_pin(layout.i2c0_scl);
        RTC_BOARD_LAYOUT.one_wire_data = encode_pin(layout.one_wire_data);
        RTC_BOARD_LAYOUT.battery_adc = encode_pin(layout.battery_adc);
        RTC_BOARD_LAYOUT.sensor_enable = encode_pin(layout.sensor_enable);
    }
}

#[cfg(feature = "esp")]
pub fn runtime_board_layout() -> Option<BoardLayout> {
    unsafe {
        if RTC_BOARD_LAYOUT.magic != RTC_LAYOUT_MAGIC {
            return None;
        }
        Some(BoardLayout {
            i2c0_sda: decode_pin(RTC_BOARD_LAYOUT.i2c0_sda),
            i2c0_scl: decode_pin(RTC_BOARD_LAYOUT.i2c0_scl),
            one_wire_data: decode_pin(RTC_BOARD_LAYOUT.one_wire_data),
            battery_adc: decode_pin(RTC_BOARD_LAYOUT.battery_adc),
            sensor_enable: decode_pin(RTC_BOARD_LAYOUT.sensor_enable),
        })
    }
}

#[cfg(not(feature = "esp"))]
use std::sync::Mutex;

#[cfg(not(feature = "esp"))]
static RUNTIME_BOARD_LAYOUT: Mutex<Option<BoardLayout>> = Mutex::new(None);

#[cfg(not(feature = "esp"))]
pub fn stage_runtime_board_layout(layout: &BoardLayout) {
    *RUNTIME_BOARD_LAYOUT.lock().unwrap() = Some(*layout);
}

#[cfg(not(feature = "esp"))]
pub fn runtime_board_layout() -> Option<BoardLayout> {
    *RUNTIME_BOARD_LAYOUT.lock().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_layout_round_trip() {
        let layout = BoardLayout::SONDE_SENSOR_NODE_REV_A;
        stage_runtime_board_layout(&layout);
        assert_eq!(runtime_board_layout(), Some(layout));
    }
}
