// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use ciborium::Value;

use crate::{DecodeError, EncodeError};

pub const BOARD_LAYOUT_KEY_I2C0_SDA: u64 = 1;
pub const BOARD_LAYOUT_KEY_I2C0_SCL: u64 = 2;
pub const BOARD_LAYOUT_KEY_ONE_WIRE_DATA: u64 = 3;
pub const BOARD_LAYOUT_KEY_BATTERY_ADC: u64 = 4;
pub const BOARD_LAYOUT_KEY_SENSOR_ENABLE: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardLayout {
    pub i2c0_sda: Option<u8>,
    pub i2c0_scl: Option<u8>,
    pub one_wire_data: Option<u8>,
    pub battery_adc: Option<u8>,
    pub sensor_enable: Option<u8>,
}

impl BoardLayout {
    pub const LEGACY_COMPAT: Self = Self {
        i2c0_sda: Some(0),
        i2c0_scl: Some(1),
        one_wire_data: None,
        battery_adc: None,
        sensor_enable: None,
    };

    pub const SONDE_SENSOR_NODE_REV_A: Self = Self {
        i2c0_sda: Some(6),
        i2c0_scl: Some(7),
        one_wire_data: Some(3),
        battery_adc: Some(2),
        sensor_enable: Some(4),
    };

    pub const ESPRESSIF_ESP32_C3_DEVKIT_M1: Self = Self {
        i2c0_sda: Some(0),
        i2c0_scl: Some(1),
        one_wire_data: None,
        battery_adc: None,
        sensor_enable: None,
    };

    pub const SPARKFUN_ESP32_C3_PRO_MICRO: Self = Self {
        i2c0_sda: Some(5),
        i2c0_scl: Some(6),
        one_wire_data: None,
        battery_adc: None,
        sensor_enable: None,
    };

    pub fn is_legacy_compat(&self) -> bool {
        self.i2c0_sda == Self::LEGACY_COMPAT.i2c0_sda
            && self.i2c0_scl == Self::LEGACY_COMPAT.i2c0_scl
            && self.one_wire_data == Self::LEGACY_COMPAT.one_wire_data
            && self.battery_adc == Self::LEGACY_COMPAT.battery_adc
            && self.sensor_enable == Self::LEGACY_COMPAT.sensor_enable
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        validate_gpio(self.i2c0_sda, "i2c0_sda")?;
        validate_gpio(self.i2c0_scl, "i2c0_scl")?;
        validate_gpio(self.one_wire_data, "one_wire_data")?;
        validate_gpio(self.battery_adc, "battery_adc")?;
        validate_gpio(self.sensor_enable, "sensor_enable")?;

        match (self.i2c0_sda, self.i2c0_scl) {
            (Some(sda), Some(scl)) if sda == scl => {
                Err("i2c0_sda and i2c0_scl must be different pins")
            }
            (Some(_), None) | (None, Some(_)) => {
                Err("i2c0_sda and i2c0_scl must both be assigned or both be unassigned")
            }
            _ => Ok(()),
        }
    }

    pub fn assigned_pins(&self) -> [Option<u8>; 5] {
        [
            self.i2c0_sda,
            self.i2c0_scl,
            self.one_wire_data,
            self.battery_adc,
            self.sensor_enable,
        ]
    }
}

fn validate_gpio(pin: Option<u8>, field: &str) -> Result<(), &'static str> {
    const MAX_GPIO: u8 = 21;
    if matches!(pin, Some(value) if value > MAX_GPIO) {
        return Err(match field {
            "i2c0_sda" => "i2c0_sda must be in GPIO range 0-21",
            "i2c0_scl" => "i2c0_scl must be in GPIO range 0-21",
            "one_wire_data" => "one_wire_data must be in GPIO range 0-21",
            "battery_adc" => "battery_adc must be in GPIO range 0-21",
            "sensor_enable" => "sensor_enable must be in GPIO range 0-21",
            _ => "GPIO value must be in range 0-21",
        });
    }
    Ok(())
}

fn value_for_pin(pin: Option<u8>) -> Value {
    match pin {
        Some(pin) => Value::Integer(pin.into()),
        None => Value::Null,
    }
}

fn decode_pin(value: &Value, field: &'static str) -> Result<Option<u8>, DecodeError> {
    match value {
        Value::Null => Ok(None),
        _ => value
            .as_integer()
            .and_then(|integer| u8::try_from(integer).ok())
            .map(Some)
            .ok_or_else(|| DecodeError::InvalidParameter(format!("{field} must be uint or null"))),
    }
}

pub fn encode_board_layout_cbor(layout: &BoardLayout) -> Result<Vec<u8>, EncodeError> {
    layout
        .validate()
        .map_err(|reason| EncodeError::InvalidParameter(reason.into()))?;

    let value = Value::Map(vec![
        (
            Value::Integer(BOARD_LAYOUT_KEY_I2C0_SDA.into()),
            value_for_pin(layout.i2c0_sda),
        ),
        (
            Value::Integer(BOARD_LAYOUT_KEY_I2C0_SCL.into()),
            value_for_pin(layout.i2c0_scl),
        ),
        (
            Value::Integer(BOARD_LAYOUT_KEY_ONE_WIRE_DATA.into()),
            value_for_pin(layout.one_wire_data),
        ),
        (
            Value::Integer(BOARD_LAYOUT_KEY_BATTERY_ADC.into()),
            value_for_pin(layout.battery_adc),
        ),
        (
            Value::Integer(BOARD_LAYOUT_KEY_SENSOR_ENABLE.into()),
            value_for_pin(layout.sensor_enable),
        ),
    ]);

    let mut buf = Vec::new();
    ciborium::into_writer(&value, &mut buf).map_err(|e| EncodeError::CborError(format!("{e}")))?;
    Ok(buf)
}

pub fn decode_board_layout_cbor(data: &[u8]) -> Result<BoardLayout, DecodeError> {
    let mut remaining = data;
    let value: Value = ciborium::from_reader(&mut remaining)
        .map_err(|e| DecodeError::CborError(format!("{e}")))?;
    if !remaining.is_empty() {
        return Err(DecodeError::InvalidParameter(
            "board_layout has trailing bytes".into(),
        ));
    }

    let map = value
        .as_map()
        .ok_or_else(|| DecodeError::InvalidParameter("board_layout must be a CBOR map".into()))?;

    let mut i2c0_sda = None;
    let mut i2c0_scl = None;
    let mut one_wire_data = None;
    let mut battery_adc = None;
    let mut sensor_enable = None;

    for (key, value) in map {
        let Some(key) = key
            .as_integer()
            .and_then(|integer| u64::try_from(integer).ok())
        else {
            return Err(DecodeError::InvalidParameter(
                "board_layout key must be an unsigned integer".into(),
            ));
        };

        match key {
            BOARD_LAYOUT_KEY_I2C0_SDA => i2c0_sda = Some(decode_pin(value, "i2c0_sda")?),
            BOARD_LAYOUT_KEY_I2C0_SCL => i2c0_scl = Some(decode_pin(value, "i2c0_scl")?),
            BOARD_LAYOUT_KEY_ONE_WIRE_DATA => {
                one_wire_data = Some(decode_pin(value, "one_wire_data")?)
            }
            BOARD_LAYOUT_KEY_BATTERY_ADC => battery_adc = Some(decode_pin(value, "battery_adc")?),
            BOARD_LAYOUT_KEY_SENSOR_ENABLE => {
                sensor_enable = Some(decode_pin(value, "sensor_enable")?)
            }
            _ => {}
        }
    }

    let layout = BoardLayout {
        i2c0_sda: i2c0_sda
            .ok_or_else(|| DecodeError::InvalidParameter("board_layout missing i2c0_sda".into()))?,
        i2c0_scl: i2c0_scl
            .ok_or_else(|| DecodeError::InvalidParameter("board_layout missing i2c0_scl".into()))?,
        one_wire_data: one_wire_data.ok_or_else(|| {
            DecodeError::InvalidParameter("board_layout missing one_wire_data".into())
        })?,
        battery_adc: battery_adc.ok_or_else(|| {
            DecodeError::InvalidParameter("board_layout missing battery_adc".into())
        })?,
        sensor_enable: sensor_enable.ok_or_else(|| {
            DecodeError::InvalidParameter("board_layout missing sensor_enable".into())
        })?,
    };

    layout
        .validate()
        .map_err(|reason| DecodeError::InvalidParameter(reason.into()))?;
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_layout_round_trip() {
        let layout = BoardLayout::SONDE_SENSOR_NODE_REV_A;
        let encoded = encode_board_layout_cbor(&layout).unwrap();
        let decoded = decode_board_layout_cbor(&encoded).unwrap();
        assert_eq!(decoded, layout);
    }

    #[test]
    fn board_layout_encoding_is_deterministic() {
        let encoded = encode_board_layout_cbor(&BoardLayout {
            i2c0_sda: Some(6),
            i2c0_scl: Some(7),
            one_wire_data: Some(3),
            battery_adc: Some(2),
            sensor_enable: Some(4),
        })
        .unwrap();
        assert_eq!(
            encoded,
            [0xA5, 0x01, 0x06, 0x02, 0x07, 0x03, 0x03, 0x04, 0x02, 0x05, 0x04]
        );
    }

    #[test]
    fn board_layout_rejects_missing_known_key() {
        let encoded = [0xA4, 0x01, 0x06, 0x02, 0x07, 0x03, 0x03, 0x05, 0x04];
        let err = decode_board_layout_cbor(&encoded).unwrap_err();
        assert_eq!(
            err,
            DecodeError::InvalidParameter("board_layout missing battery_adc".into())
        );
    }

    #[test]
    fn board_layout_rejects_half_i2c_assignment() {
        let err = BoardLayout {
            i2c0_sda: Some(6),
            i2c0_scl: None,
            one_wire_data: None,
            battery_adc: None,
            sensor_enable: None,
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            err,
            "i2c0_sda and i2c0_scl must both be assigned or both be unassigned"
        );
    }
}
