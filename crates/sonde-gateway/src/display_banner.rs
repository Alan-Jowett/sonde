// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::mono_font::ascii::FONT_8X13_BOLD;
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
const LINE_SPACING: i32 = 4;

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

fn centered_text_x(text: &str) -> i32 {
    let character_width = FONT_8X13_BOLD.character_size.width as i32;
    let text_width = (text.chars().count() as i32) * character_width;
    ((FRAMEBUFFER_WIDTH as i32 - text_width).max(0)) / 2
}

pub fn render_display_message(lines: &[&str]) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
    if lines.is_empty() {
        return [0u8; DISPLAY_FRAME_BODY_SIZE];
    }

    let line_height = FONT_8X13_BOLD.character_size.height as i32;
    let block_height =
        line_height * lines.len() as i32 + LINE_SPACING * (lines.len().saturating_sub(1) as i32);
    let top = ((FRAMEBUFFER_HEIGHT as i32 - block_height).max(0)) / 2;
    let style = MonoTextStyle::new(&FONT_8X13_BOLD, BinaryColor::On);
    let mut framebuffer = Framebuffer::new();

    for (index, line) in lines.iter().enumerate() {
        let baseline =
            top + index as i32 * (line_height + LINE_SPACING) + FONT_8X13_BOLD.baseline as i32;
        let _ = Text::new(line, Point::new(centered_text_x(line), baseline), style)
            .draw(&mut framebuffer);
    }

    framebuffer.into_bytes()
}

pub fn render_gateway_version_banner(version: &str) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
    let line2 = format!("v{version}");
    render_display_message(&["Sonde Gateway", &line2])
}

pub async fn send_display_message(
    transport: &UsbEspNowTransport,
    lines: &[&str],
) -> Result<(), TransportError> {
    transport
        .send_display_frame(render_display_message(lines))
        .await
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
    fn gateway_banner_leaves_outer_margins() {
        let framebuffer = render_gateway_version_banner("0.4.0");
        let first_nonzero = framebuffer
            .iter()
            .position(|byte| *byte != 0)
            .expect("banner should render pixels");
        let last_nonzero = framebuffer
            .iter()
            .rposition(|byte| *byte != 0)
            .expect("banner should render pixels");
        assert!(
            first_nonzero > 0,
            "left/top margin should contain at least one empty byte"
        );
        assert!(
            last_nonzero + 1 < framebuffer.len(),
            "right/bottom margin should not consume the full framebuffer"
        );
    }

    #[test]
    fn gateway_banner_renders_across_two_vertical_regions() {
        let framebuffer = render_gateway_version_banner("0.4.0");
        let half = framebuffer.len() / 2;
        assert!(
            framebuffer[..half].iter().any(|byte| *byte != 0),
            "top half should contain the first line"
        );
        assert!(
            framebuffer[half..].iter().any(|byte| *byte != 0),
            "bottom half should contain the second line"
        );
    }

    #[test]
    fn pairing_message_renders_visible_pixels() {
        let framebuffer = render_display_message(&["Pairing"]);
        assert!(
            framebuffer.iter().any(|byte| *byte != 0),
            "rendered pairing message must set at least one pixel"
        );
    }

    #[test]
    fn passkey_message_renders_across_two_vertical_regions() {
        let framebuffer = render_display_message(&["Pin", "123456"]);
        let half = framebuffer.len() / 2;
        assert!(
            framebuffer[..half].iter().any(|byte| *byte != 0),
            "top half should contain the first line"
        );
        assert!(
            framebuffer[half..].iter().any(|byte| *byte != 0),
            "bottom half should contain the second line"
        );
    }
}
