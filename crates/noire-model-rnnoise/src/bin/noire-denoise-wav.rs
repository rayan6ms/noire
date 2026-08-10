//! Development-only latency-compensated WAV runner.

use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use noire_model_rnnoise::{RNNOISE_SAMPLE_RATE_HZ, denoise_latency_compensated};

fn main() -> Result<(), Box<dyn Error>> {
    let (input_path, output_path) = parse_paths(std::env::args_os().skip(1))?;
    if input_path == output_path {
        return Err("input and output WAV paths must differ".into());
    }

    let input = read_wav(File::open(&input_path)?)?;
    let output = denoise_latency_compensated(&input)?;
    write_wav(File::create(&output_path)?, &output)?;
    Ok(())
}

fn parse_paths(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let input = arguments.next().ok_or("missing input WAV path")?;
    let output = arguments.next().ok_or("missing output WAV path")?;
    if arguments.next().is_some() {
        return Err("usage: noire-denoise-wav <input.wav> <output.wav>".into());
    }
    Ok((PathBuf::from(input), PathBuf::from(output)))
}

fn read_wav(reader: impl Read) -> Result<Vec<f32>, Box<dyn Error>> {
    let mut reader = WavReader::new(reader)?;
    let specification = reader.spec();
    validate_specification(specification)?;

    let samples = match specification.sample_format {
        SampleFormat::Int => reader
            .samples::<i16>()
            .map(|sample| sample.map(normalize_i16))
            .collect::<Result<Vec<_>, _>>()?,
        SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
    };
    if samples
        .iter()
        .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        return Err("input WAV contains a non-finite or out-of-range sample".into());
    }
    Ok(samples)
}

fn validate_specification(specification: WavSpec) -> Result<(), Box<dyn Error>> {
    if specification.channels != 1 || specification.sample_rate != RNNOISE_SAMPLE_RATE_HZ {
        return Err("input must be mono at 48 kHz".into());
    }
    let supported = matches!(
        (specification.sample_format, specification.bits_per_sample),
        (SampleFormat::Int, 16) | (SampleFormat::Float, 32)
    );
    if !supported {
        return Err("input must use signed 16-bit PCM or 32-bit float samples".into());
    }
    Ok(())
}

fn write_wav(writer: impl Write + Seek, samples: &[f32]) -> Result<(), Box<dyn Error>> {
    let specification = WavSpec {
        channels: 1,
        sample_rate: RNNOISE_SAMPLE_RATE_HZ,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::new(writer, specification)?;
    for sample in samples {
        if !sample.is_finite() {
            return Err("offline adapter produced a non-finite sample".into());
        }
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn normalize_i16(sample: i16) -> f32 {
    if sample.is_negative() {
        f32::from(sample) / 32_768.0
    } else {
        f32::from(sample) / 32_767.0
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

    use super::{read_wav, write_wav};

    #[test]
    fn reads_signed_pcm_endpoints_and_writes_float_output() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut encoded = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(
                &mut encoded,
                WavSpec {
                    channels: 1,
                    sample_rate: 48_000,
                    bits_per_sample: 16,
                    sample_format: SampleFormat::Int,
                },
            )?;
            writer.write_sample(i16::MIN)?;
            writer.write_sample(0_i16)?;
            writer.write_sample(i16::MAX)?;
            writer.finalize()?;
        }

        let samples = read_wav(Cursor::new(encoded.into_inner()))?;
        assert_eq!(samples, [-1.0, 0.0, 1.0]);

        let mut output = Cursor::new(Vec::new());
        write_wav(&mut output, &samples)?;
        let reader = WavReader::new(Cursor::new(output.into_inner()))?;
        assert_eq!(reader.spec().sample_format, SampleFormat::Float);
        assert_eq!(reader.duration(), 3);
        Ok(())
    }

    #[test]
    fn rejects_wrong_rate() -> Result<(), Box<dyn std::error::Error>> {
        let mut encoded = Cursor::new(Vec::new());
        {
            let writer = WavWriter::new(
                &mut encoded,
                WavSpec {
                    channels: 1,
                    sample_rate: 44_100,
                    bits_per_sample: 32,
                    sample_format: SampleFormat::Float,
                },
            )?;
            writer.finalize()?;
        }
        assert!(read_wav(Cursor::new(encoded.into_inner())).is_err());
        Ok(())
    }
}
