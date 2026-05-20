use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct ExifData {
    pub captured_at: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture: Option<f32>,
    pub iso: Option<u32>,
    pub shutter_seconds: Option<f32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<u16>,
}

/// Extract EXIF from a RAW file using rawler.
pub fn read_exif_raw(path: &Path) -> Result<ExifData, crate::error::IngestError> {
    use rawler::{decoders::RawDecodeParams, rawsource::RawSource};

    let raw_source = RawSource::new(path).map_err(|e| crate::error::IngestError::Exif {
        path: path.to_owned(),
        reason: e.to_string(),
    })?;
    let decoder =
        rawler::get_decoder(&raw_source).map_err(|e| crate::error::IngestError::Exif {
            path: path.to_owned(),
            reason: e.to_string(),
        })?;
    let params = RawDecodeParams::default();
    let metadata = decoder.raw_metadata(&raw_source, &params).map_err(|e| {
        crate::error::IngestError::Exif {
            path: path.to_owned(),
            reason: e.to_string(),
        }
    })?;

    let exif = &metadata.exif;

    let captured_at = exif.date_time_original.as_deref().and_then(parse_datetime);
    let focal_length_mm = exif.focal_length.map(|r| r.n as f32 / r.d as f32);
    let aperture = exif.fnumber.map(|r| r.n as f32 / r.d as f32);
    // Prefer iso_speed_ratings (u16), fall back to iso_speed (u32).
    let iso = exif.iso_speed_ratings.map(|v| v as u32).or(exif.iso_speed);
    let shutter_seconds = exif.exposure_time.map(|r| r.n as f32 / r.d as f32);
    let orientation = exif.orientation;

    Ok(ExifData {
        captured_at,
        camera_make: Some(metadata.make.clone()).filter(|s| !s.is_empty()),
        camera_model: Some(metadata.model.clone()).filter(|s| !s.is_empty()),
        lens_model: exif.lens_model.clone(),
        focal_length_mm,
        aperture,
        iso,
        shutter_seconds,
        width: None, // filled from preview dimensions in ingest
        height: None,
        orientation,
    })
}

/// Extract EXIF from a JPEG file using kamadak-exif.
pub fn read_exif_jpg(path: &Path) -> Result<ExifData, crate::error::IngestError> {
    use std::{fs::File, io::BufReader};

    let f = File::open(path).map_err(|e| crate::error::IngestError::Io {
        path: path.to_owned(),
        source: e,
    })?;
    let mut reader = BufReader::new(f);
    let exif_reader = exif::Reader::new();
    let exif = match exif_reader.read_from_container(&mut reader) {
        Ok(e) => e,
        Err(e) => {
            return Err(crate::error::IngestError::Exif {
                path: path.to_owned(),
                reason: e.to_string(),
            })
        }
    };

    let get_str = |tag| -> Option<String> {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
    };

    let get_rational = |tag| -> Option<f32> {
        exif.get_field(tag, exif::In::PRIMARY).and_then(|f| {
            if let exif::Value::Rational(ref v) = f.value {
                v.first().map(|r| r.num as f32 / r.denom as f32)
            } else {
                None
            }
        })
    };

    let get_u32 = |tag| -> Option<u32> {
        exif.get_field(tag, exif::In::PRIMARY).and_then(|f| {
            if let exif::Value::Short(ref v) = f.value {
                v.first().copied().map(|x| x as u32)
            } else if let exif::Value::Long(ref v) = f.value {
                v.first().copied()
            } else {
                None
            }
        })
    };

    let captured_at = get_str(exif::Tag::DateTimeOriginal)
        .as_deref()
        .and_then(parse_datetime);

    let width = get_u32(exif::Tag::PixelXDimension);
    let height = get_u32(exif::Tag::PixelYDimension);
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| {
            if let exif::Value::Short(ref v) = f.value {
                v.first().copied()
            } else {
                None
            }
        });

    Ok(ExifData {
        captured_at,
        camera_make: get_str(exif::Tag::Make),
        camera_model: get_str(exif::Tag::Model),
        lens_model: get_str(exif::Tag::LensModel),
        focal_length_mm: get_rational(exif::Tag::FocalLength),
        aperture: get_rational(exif::Tag::FNumber),
        iso: get_u32(exif::Tag::PhotographicSensitivity),
        shutter_seconds: get_rational(exif::Tag::ExposureTime),
        width,
        height,
        orientation,
    })
}

/// Parse an EXIF datetime string `"YYYY:MM:DD HH:MM:SS"` to Unix timestamp (seconds).
fn parse_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let year: i32 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let min: u32 = s[14..16].parse().ok()?;
    let sec: u32 = s[17..19].parse().ok()?;

    let days = days_from_epoch(year, month, day)?;
    let secs = days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    Some(secs)
}

/// Number of days from the Unix epoch (1970-01-01) to the given date.
///
/// Uses the algorithm from <https://howardhinnant.github.io/date_algorithms.html>.
fn days_from_epoch(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let m = month as i64;
    let d = day as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // year-of-era [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datetime_known() {
        // 2023-06-15 12:00:00 UTC — pre-calculated
        let ts = parse_datetime("2023:06:15 12:00:00").unwrap();
        // Days from 1970-01-01 to 2023-06-15 = 19523; * 86400 + 43200
        assert_eq!(ts, 19523 * 86400 + 12 * 3600);
    }

    #[test]
    fn parse_datetime_invalid() {
        assert!(parse_datetime("bad string").is_none());
        assert!(parse_datetime("").is_none());
    }
}
