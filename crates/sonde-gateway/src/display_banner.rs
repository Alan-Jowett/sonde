// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::mono_font::ascii::FONT_5X7;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Pixel, Point};
use embedded_graphics::text::Text;
use embedded_graphics::Drawable;
use sonde_protocol::modem::DISPLAY_FRAME_BODY_SIZE;

use crate::modem::UsbEspNowTransport;
use crate::transport::TransportError;

const FRAMEBUFFER_WIDTH: u32 = 128;
const FRAMEBUFFER_HEIGHT: u32 = 64;
const ROW_BYTES: usize = (FRAMEBUFFER_WIDTH as usize) / 8;
const CHARACTER_ADVANCE: i32 = 6;

struct Framebuffer {
    bytes: [u8; DISPLAY_FRAME_BODY_SIZE],
}

impl Framebuffer {
    fn new() -> Self {
        Self {
            bytes: [0u8; DISPLAY_FRAME_BODY_SIZE],
        }
    }

    fn into_bytes(self) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
        self.bytes
    }

    fn set_pixel(&mut self, x: u32, y: u32) {
        if x >= FRAMEBUFFER_WIDTH || y >= FRAMEBUFFER_HEIGHT {
            return;
        }
        let index = (y as usize * ROW_BYTES) + (x as usize / 8);
        let bit = 7 - (x as usize % 8);
        self.bytes[index] |= 1 << bit;
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(FRAMEBUFFER_WIDTH, FRAMEBUFFER_HEIGHT)
    }
}

impl DrawTarget for Framebuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if color != BinaryColor::On {
                continue;
            }
            let Ok(x) = u32::try_from(point.x) else {
                continue;
            };
            let Ok(y) = u32::try_from(point.y) else {
                continue;
            };
            self.set_pixel(x, y);
        }
        Ok(())
    }
}

fn gateway_banner_text(version: &str) -> String {
    format!("Sonde Gateway v{version}")
}

pub fn render_gateway_version_banner(version: &str) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
    let text = gateway_banner_text(version);
    let text_width = (text.chars().count() as i32) * CHARACTER_ADVANCE - 1;
    let x = ((FRAMEBUFFER_WIDTH as i32 - text_width).max(0)) / 2;
    let y = ((FRAMEBUFFER_HEIGHT as i32 - FONT_5X7.character_size.height as i32).max(0)) / 2
        + FONT_5X7.baseline as i32;

    let style = MonoTextStyle::new(&FONT_5X7, BinaryColor::On);
    let mut framebuffer = Framebuffer::new();
    let _ = Text::new(&text, Point::new(x, y), style).draw(&mut framebuffer);
    framebuffer.into_bytes()
}

pub async fn send_gateway_version_banner(
    transport: &UsbEspNowTransport,
) -> Result<(), TransportError> {
    transport
        .send_display_frame(render_gateway_version_banner(env!("CARGO_PKG_VERSION")))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_banner_renders_visible_pixels() {
        let framebuffer = render_gateway_version_banner("0.4.0");
        assert!(
            framebuffer.iter().any(|byte| *byte != 0),
            "rendered banner must set at least one pixel"
        );
    }

    #[test]
    fn gateway_banner_is_centered_with_margins() {
        let framebuffer = render_gateway_version_banner("0.4.0");
        let first_nonzero = framebuffer
            .iter()
            .position(|byte| *byte != 0)
            .expect("banner should render pixels");
        let last_nonzero = framebuffer
            .iter()
            .rposition(|byte| *byte != 0)
            .expect("banner should render pixels");
        assert!(first_nonzero > 0, "left/top margin should not be empty");
        assert!(
            last_nonzero + 1 < framebuffer.len(),
            "right/bottom margin should not consume the full framebuffer"
        );
    }
}
