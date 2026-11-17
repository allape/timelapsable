//! Color math, white-balance parsing and fast per-channel lookup tables.

use std::str::FromStr;

/// Requested white-balance mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WhiteBalance {
    /// Analyze the sequence and smooth the estimated neutral balance.
    Auto,
    /// Bias the neutral result toward a Planckian color temperature.
    Temperature(f32),
}

impl WhiteBalance {
    /// The temperature bias factors, normalized so that 6500 K is neutral.
    pub fn temperature_factors(self) -> [f32; 3] {
        match self {
            Self::Auto => [1.0, 1.0, 1.0],
            Self::Temperature(kelvin) => temperature_factors(kelvin),
        }
    }
}

impl FromStr for WhiteBalance {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        let preset = match trimmed.to_ascii_lowercase().as_str() {
            "daylight" => Some(5600.0),
            "cloudy" => Some(6500.0),
            "shade" => Some(7500.0),
            "tungsten" | "incandescent" => Some(3200.0),
            "fluorescent" => Some(4000.0),
            "flash" => Some(5500.0),
            _ => None,
        };
        if let Some(kelvin) = preset {
            return Ok(Self::Temperature(kelvin));
        }

        let numeric = trimmed
            .trim_end_matches(['K', 'k'])
            .trim()
            .parse::<f32>()
            .map_err(|_| {
                format!(
                    "invalid white balance '{value}'; expected auto, a preset name, or Kelvin \
                     in 1500..=15000 (for example 5600K)"
                )
            })?;
        if !numeric.is_finite() || !(1500.0..=15000.0).contains(&numeric) {
            return Err(format!(
                "white balance temperature {numeric} K is outside the supported range 1500..=15000 K"
            ));
        }
        Ok(Self::Temperature(numeric))
    }
}

/// Converts an 8-bit sRGB value to linear light in 0..=1.
pub fn srgb_to_linear(value: u8) -> f32 {
    let encoded = value as f32 / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Converts linear light to sRGB in 0..=1.
pub fn linear_to_srgb(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Approximates the display RGB of a black body at `kelvin` (Tanner Helland's
/// widely used fit). Values are normalized so green is 1.
fn blackbody_rgb(kelvin: f32) -> [f32; 3] {
    let temperature = (kelvin.clamp(1000.0, 40000.0) / 100.0) as f64;
    let red = if temperature <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (temperature - 60.0).powf(-0.133_204_759_2)
    };
    let green = if temperature <= 66.0 {
        99.470_802_586_1 * temperature.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (temperature - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if temperature >= 66.0 {
        255.0
    } else if temperature <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (temperature - 10.0).ln() - 305.044_792_730_7
    };
    let red = red.clamp(0.0, 255.0) / 255.0;
    let green = green.clamp(0.0, 255.0) / 255.0;
    let blue = blue.clamp(0.0, 255.0) / 255.0;
    let green_scale = green.max(1e-6);
    [(red / green_scale) as f32, 1.0, (blue / green_scale) as f32]
}

/// Returns per-channel multipliers relative to a neutral 6500 K reference.
///
/// Lower temperatures produce warmer output; higher temperatures produce cooler
/// output. The result is clamped so extreme temperatures cannot destroy detail.
pub fn temperature_factors(kelvin: f32) -> [f32; 3] {
    if (kelvin - 6500.0).abs() < 1.0 {
        return [1.0, 1.0, 1.0];
    }
    let target = blackbody_rgb(kelvin);
    let reference = blackbody_rgb(6500.0);
    [
        (target[0] / reference[0]).clamp(0.35, 2.8),
        1.0,
        (target[2] / reference[2]).clamp(0.35, 2.8),
    ]
}

/// Builds a 256-entry sRGB lookup table for one channel.
///
/// The gain is applied in linear light, then the result is encoded back to sRGB
/// and the manual brightness offset is added. Setting `offset` to -1.0 yields
/// pure black and +1.0 yields pure white for every input.
pub fn build_channel_lut(gain: f32, offset: f32) -> [u8; 256] {
    let mut table = [0_u8; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let linear = srgb_to_linear(index as u8) * gain.max(0.0);
        let encoded = linear_to_srgb(linear) + offset;
        *slot = (encoded.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_temperature_and_presets() {
        assert_eq!("auto".parse(), Ok(WhiteBalance::Auto));
        assert_eq!("5600".parse(), Ok(WhiteBalance::Temperature(5600.0)));
        assert_eq!("5600K".parse(), Ok(WhiteBalance::Temperature(5600.0)));
        assert_eq!("daylight".parse(), Ok(WhiteBalance::Temperature(5600.0)));
        assert!("100".parse::<WhiteBalance>().is_err());
        assert!("nonsense".parse::<WhiteBalance>().is_err());
    }

    #[test]
    fn neutral_at_6500k() {
        assert_eq!(temperature_factors(6500.0), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn lower_temperature_is_warmer() {
        let factors = temperature_factors(3200.0);
        assert!(factors[0] > 1.0);
        assert!(factors[2] < 1.0);
    }

    #[test]
    fn brightness_extremes_are_black_and_white() {
        let black = build_channel_lut(1.0, -1.0);
        let white = build_channel_lut(1.0, 1.0);
        assert!(black.iter().all(|value| *value == 0));
        assert!(white.iter().all(|value| *value == 255));
    }

    #[test]
    fn srgb_round_trip_is_close() {
        for value in 0_u8..=255 {
            let encoded = linear_to_srgb(srgb_to_linear(value));
            assert!((encoded * 255.0 - value as f32).abs() < 1.0);
        }
    }
}
