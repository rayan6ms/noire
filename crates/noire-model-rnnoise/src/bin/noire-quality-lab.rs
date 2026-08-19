//! Development-only WAV runner for fixed-model quality experiments.

#![allow(clippy::cast_precision_loss)]

use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use noire_dsp::MODEL_FRAME_SAMPLES;
use noire_model::{DenoiserFactory, FrameStats};
use noire_model_rnnoise::{
    EnhancedRnnoiseConfig, EnhancedRnnoiseFactory, RNNOISE_SAMPLE_RATE_HZ, RnnoiseCandidateFactory,
    RnnoiseFactory,
};

const DEFAULT_STRENGTH: f32 = 1.0;
const DEFAULT_VAD_LOW: f32 = 0.20;
const DEFAULT_VAD_HIGH: f32 = 0.80;
const SPEECH_ATTACK_SECONDS: f32 = 0.010;
const NOISE_RELEASE_SECONDS: f32 = 0.100;

#[derive(Clone, Debug, PartialEq)]
struct Options {
    high_pass_hz: Option<f32>,
    model_only_high_pass: bool,
    model_gain: f32,
    passes: usize,
    model_file: Option<PathBuf>,
    enhanced: bool,
    dereverb_strength: f32,
    strength: f32,
    adaptive: Option<AdaptiveMix>,
    input_path: PathBuf,
    output_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AdaptiveMix {
    speech_strength: f32,
    noise_strength: f32,
    vad_low: f32,
    vad_high: f32,
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options(std::env::args_os().skip(1))?;
    if options.input_path == options.output_path {
        return Err("input and output WAV paths must differ".into());
    }

    let mut dry = read_wav(File::open(&options.input_path)?)?;
    let mut model_input = dry.clone();
    if let Some(cutoff_hz) = options.high_pass_hz {
        apply_high_pass(&mut model_input, cutoff_hz);
        if !options.model_only_high_pass {
            dry.clone_from(&model_input);
        }
    }

    let candidate_bytes = options.model_file.as_ref().map(std::fs::read).transpose()?;
    let (mut wet, vad) = run_selected_model(
        &model_input,
        options.model_gain,
        candidate_bytes.as_deref(),
        options.enhanced,
        options.dereverb_strength,
    )?;
    for _ in 1..options.passes {
        (wet, _) = run_selected_model(
            &wet,
            1.0,
            candidate_bytes.as_deref(),
            options.enhanced,
            options.dereverb_strength,
        )?;
    }
    let output = if let Some(adaptive) = options.adaptive {
        adaptive_mix(&dry, &wet, &vad, adaptive)
    } else {
        fixed_mix(&dry, &wet, options.strength)
    };
    write_wav(File::create(&options.output_path)?, &output)?;
    Ok(())
}

fn parse_options(arguments: impl Iterator<Item = OsString>) -> Result<Options, Box<dyn Error>> {
    let mut arguments = arguments.peekable();
    let mut high_pass_hz = None;
    let mut model_only_high_pass = false;
    let mut model_gain = 1.0_f32;
    let mut passes = 1_usize;
    let mut model_file = None;
    let mut enhanced = false;
    let mut dereverb_strength = EnhancedRnnoiseConfig::default().dereverb_strength;
    let mut strength = DEFAULT_STRENGTH;
    let mut speech_strength = None;
    let mut noise_strength = None;
    let mut vad_low = DEFAULT_VAD_LOW;
    let mut vad_high = DEFAULT_VAD_HIGH;
    let mut paths = Vec::new();

    while let Some(argument) = arguments.next() {
        if argument == "--live" {
            high_pass_hz = Some(60.0);
        } else if argument == "--model-high-pass" {
            high_pass_hz = Some(parse_f32(arguments.next(), "--model-high-pass")?);
            model_only_high_pass = true;
        } else if argument == "--high-pass" {
            high_pass_hz = Some(parse_f32(arguments.next(), "--high-pass")?);
        } else if argument == "--model-gain" {
            model_gain = parse_f32(arguments.next(), "--model-gain")?;
        } else if argument == "--passes" {
            passes = parse_usize(arguments.next(), "--passes")?;
        } else if argument == "--model-file" {
            model_file = Some(PathBuf::from(
                arguments.next().ok_or("missing value for --model-file")?,
            ));
        } else if argument == "--enhanced" {
            enhanced = true;
        } else if argument == "--dereverb-strength" {
            dereverb_strength = parse_f32(arguments.next(), "--dereverb-strength")?;
        } else if argument == "--strength" {
            strength = parse_f32(arguments.next(), "--strength")?;
        } else if argument == "--speech-strength" {
            speech_strength = Some(parse_f32(arguments.next(), "--speech-strength")?);
        } else if argument == "--noise-strength" {
            noise_strength = Some(parse_f32(arguments.next(), "--noise-strength")?);
        } else if argument == "--vad-low" {
            vad_low = parse_f32(arguments.next(), "--vad-low")?;
        } else if argument == "--vad-high" {
            vad_high = parse_f32(arguments.next(), "--vad-high")?;
        } else if argument.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option: {}", argument.to_string_lossy()).into());
        } else {
            paths.push(PathBuf::from(argument));
        }
    }

    if paths.len() != 2 {
        return Err(usage().into());
    }
    if !model_gain.is_finite() || !(0.125..=8.0).contains(&model_gain) {
        return Err("--model-gain must be finite and within 0.125..=8".into());
    }
    if high_pass_hz.is_some_and(|cutoff| !cutoff.is_finite() || !(1.0..=200.0).contains(&cutoff)) {
        return Err("--high-pass must be finite and within 1..=200 Hz".into());
    }
    if !(1..=2).contains(&passes) {
        return Err("--passes must be 1 or 2".into());
    }
    validate_model_selection(model_file.as_ref(), enhanced)?;
    validate_unit(strength, "--strength")?;
    validate_unit(dereverb_strength, "--dereverb-strength")?;
    validate_unit(vad_low, "--vad-low")?;
    validate_unit(vad_high, "--vad-high")?;
    if vad_low >= vad_high {
        return Err("--vad-low must be less than --vad-high".into());
    }

    let adaptive = match (speech_strength, noise_strength) {
        (None, None) => None,
        (Some(speech_strength), Some(noise_strength)) => {
            validate_unit(speech_strength, "--speech-strength")?;
            validate_unit(noise_strength, "--noise-strength")?;
            Some(AdaptiveMix {
                speech_strength,
                noise_strength,
                vad_low,
                vad_high,
            })
        }
        _ => {
            return Err("--speech-strength and --noise-strength must be specified together".into());
        }
    };

    Ok(Options {
        high_pass_hz,
        model_only_high_pass,
        model_gain,
        passes,
        model_file,
        enhanced,
        dereverb_strength,
        strength,
        adaptive,
        input_path: paths.remove(0),
        output_path: paths.remove(0),
    })
}

fn validate_model_selection(
    model_file: Option<&PathBuf>,
    enhanced: bool,
) -> Result<(), Box<dyn Error>> {
    if model_file.is_some() && enhanced {
        Err("--model-file and --enhanced cannot be combined".into())
    } else {
        Ok(())
    }
}

fn usage() -> &'static str {
    "usage: noire-quality-lab [--live | --high-pass HZ | --model-high-pass HZ] \
     [--model-gain G] [--passes 1|2] [--model-file WEIGHTS.rnn | \
     --enhanced [--dereverb-strength S]] \
     [--strength S | --speech-strength S --noise-strength S \
     [--vad-low V --vad-high V]] <input.wav> <output.wav>"
}

fn parse_f32(value: Option<OsString>, option: &str) -> Result<f32, Box<dyn Error>> {
    value
        .ok_or_else(|| format!("missing value for {option}"))?
        .to_string_lossy()
        .parse()
        .map_err(|_| format!("invalid value for {option}").into())
}

fn parse_usize(value: Option<OsString>, option: &str) -> Result<usize, Box<dyn Error>> {
    value
        .ok_or_else(|| format!("missing value for {option}"))?
        .to_string_lossy()
        .parse()
        .map_err(|_| format!("invalid value for {option}").into())
}

fn validate_unit(value: f32, option: &str) -> Result<(), Box<dyn Error>> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{option} must be finite and within 0..=1").into())
    }
}

#[cfg(test)]
fn run_model(input: &[f32], model_gain: f32) -> Result<(Vec<f32>, Vec<f32>), Box<dyn Error>> {
    run_selected_model(input, model_gain, None, false, 0.0)
}

fn run_selected_model(
    input: &[f32],
    model_gain: f32,
    candidate_bytes: Option<&[u8]>,
    enhanced: bool,
    dereverb_strength: f32,
) -> Result<(Vec<f32>, Vec<f32>), Box<dyn Error>> {
    let factory: Box<dyn DenoiserFactory> = if let Some(bytes) = candidate_bytes {
        Box::new(RnnoiseCandidateFactory::from_bytes(bytes)?)
    } else if enhanced {
        Box::new(EnhancedRnnoiseFactory::new(EnhancedRnnoiseConfig {
            dereverb_strength,
        })?)
    } else {
        Box::new(RnnoiseFactory::new()?)
    };
    let delay_samples = factory.descriptor().delay_samples();
    let mut model = factory.create()?;
    let input_frames = input.len().div_ceil(MODEL_FRAME_SAMPLES);
    let output_frames = input_frames + delay_samples.div_ceil(MODEL_FRAME_SAMPLES);
    let mut raw_output = Vec::with_capacity(output_frames * MODEL_FRAME_SAMPLES);
    let mut vad = Vec::with_capacity(output_frames);
    let mut frame = [0.0; MODEL_FRAME_SAMPLES];
    let mut model_output = [0.0; MODEL_FRAME_SAMPLES];

    for frame_index in 0..output_frames {
        frame.fill(0.0);
        if frame_index < input_frames {
            let start = frame_index * MODEL_FRAME_SAMPLES;
            let end = (start + MODEL_FRAME_SAMPLES).min(input.len());
            for (source, destination) in input[start..end].iter().zip(frame.iter_mut()) {
                *destination = (*source * model_gain).clamp(-1.0, 1.0);
            }
        }
        let stats: FrameStats = model.process_frame(&frame, &mut model_output)?;
        raw_output.extend(model_output.iter().map(|sample| sample / model_gain));
        vad.push(stats.vad_probability());
    }

    let output_end = delay_samples
        .checked_add(input.len())
        .ok_or("output length overflow")?;
    let output = raw_output
        .get(delay_samples..output_end)
        .ok_or("model output did not contain the declared delay")?
        .to_vec();
    Ok((output, vad))
}

fn fixed_mix(dry: &[f32], wet: &[f32], strength: f32) -> Vec<f32> {
    dry.iter()
        .zip(wet)
        .map(|(dry, wet)| linear_mix(*dry, *wet, strength))
        .collect()
}

fn adaptive_mix(dry: &[f32], wet: &[f32], vad: &[f32], policy: AdaptiveMix) -> Vec<f32> {
    let mut output = Vec::with_capacity(dry.len());
    let mut strength = policy.noise_strength;
    let speech_attack = smoothing_coefficient(SPEECH_ATTACK_SECONDS);
    let noise_release = smoothing_coefficient(NOISE_RELEASE_SECONDS);

    for (sample_index, (dry, wet)) in dry.iter().zip(wet).enumerate() {
        let output_frame = sample_index / MODEL_FRAME_SAMPLES;
        // The cropped output frame is delayed by one model frame, so its
        // matching VAD is the preceding process call's statistic. That value
        // remains available to a causal live callback without lookahead.
        let probability = vad
            .get(output_frame)
            .copied()
            .or_else(|| vad.last().copied())
            .unwrap_or(0.0);
        let speech = smoothstep(policy.vad_low, policy.vad_high, probability);
        let target =
            policy.noise_strength + (policy.speech_strength - policy.noise_strength) * speech;
        let coefficient = if target < strength {
            speech_attack
        } else {
            noise_release
        };
        strength = target + coefficient * (strength - target);
        output.push(linear_mix(*dry, *wet, strength));
    }
    output
}

fn smoothing_coefficient(seconds: f32) -> f32 {
    (-1.0 / (seconds * RNNOISE_SAMPLE_RATE_HZ as f32)).exp()
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let position = ((value - low) / (high - low)).clamp(0.0, 1.0);
    position * position * (3.0 - 2.0 * position)
}

fn linear_mix(dry: f32, wet: f32, strength: f32) -> f32 {
    (dry * (1.0 - strength) + wet * strength).clamp(-1.0, 1.0)
}

fn apply_high_pass(samples: &mut [f32], cutoff_hz: f32) {
    let coefficient =
        (-2.0 * core::f32::consts::PI * cutoff_hz / RNNOISE_SAMPLE_RATE_HZ as f32).exp();
    let mut previous_input = 0.0_f32;
    let mut previous_output = 0.0_f32;
    for sample in samples {
        let input = *sample;
        let output = input - previous_input + coefficient * previous_output;
        previous_input = input;
        previous_output = output;
        *sample = output;
    }
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
    if !matches!(
        (specification.sample_format, specification.bits_per_sample),
        (SampleFormat::Int, 16) | (SampleFormat::Float, 32)
    ) {
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
            return Err("quality lab produced a non-finite sample".into());
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
    #![allow(clippy::float_cmp)]

    use noire_dsp::DcBlocker;

    use super::{
        AdaptiveMix, Options, apply_high_pass, fixed_mix, parse_options, run_model, smoothstep,
    };

    #[test]
    fn parses_fixed_and_adaptive_options() -> Result<(), Box<dyn std::error::Error>> {
        let fixed = parse_options(
            [
                "--live",
                "--model-gain",
                "0.5",
                "--strength",
                "0.75",
                "in.wav",
                "out.wav",
            ]
            .into_iter()
            .map(Into::into),
        )?;
        assert_eq!(fixed.high_pass_hz, Some(60.0));
        assert!(!fixed.model_only_high_pass);
        assert_eq!(fixed.model_gain, 0.5);
        assert!(!fixed.enhanced);
        assert_eq!(fixed.strength, 0.75);
        assert_eq!(fixed.adaptive, None);

        let adaptive = parse_options(
            [
                "--speech-strength",
                "0.5",
                "--noise-strength",
                "1",
                "--vad-low",
                "0.1",
                "--vad-high",
                "0.9",
                "in.wav",
                "out.wav",
            ]
            .into_iter()
            .map(Into::into),
        )?;
        assert_eq!(
            adaptive.adaptive,
            Some(AdaptiveMix {
                speech_strength: 0.5,
                noise_strength: 1.0,
                vad_low: 0.1,
                vad_high: 0.9,
            })
        );

        let enhanced = parse_options(
            [
                "--enhanced",
                "--dereverb-strength",
                "0.4",
                "in.wav",
                "out.wav",
            ]
            .into_iter()
            .map(Into::into),
        )?;
        assert!(enhanced.enhanced);
        assert_eq!(enhanced.dereverb_strength, 0.4);
        Ok(())
    }

    #[test]
    fn model_gain_is_level_compensated_and_sample_exact() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut input = vec![0.0; 997];
        input[240] = 0.5;
        let (output, vad) = run_model(&input, 0.5)?;
        assert_eq!(output.len(), input.len());
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(vad.len() > input.len().div_ceil(480));
        Ok(())
    }

    #[test]
    fn mixing_endpoints_and_vad_curve_are_bounded() {
        assert_eq!(fixed_mix(&[0.25], &[-0.5], 0.0), [0.25]);
        assert_eq!(fixed_mix(&[0.25], &[-0.5], 1.0), [-0.5]);
        assert_eq!(smoothstep(0.2, 0.8, 0.0), 0.0);
        assert_eq!(smoothstep(0.2, 0.8, 1.0), 1.0);
        assert!((0.0..=1.0).contains(&smoothstep(0.2, 0.8, 0.5)));
    }

    #[test]
    fn rejects_partial_adaptive_configuration() {
        let result = parse_options(
            ["--speech-strength", "0.5", "in.wav", "out.wav"]
                .into_iter()
                .map(Into::into),
        );
        assert!(result.is_err());
    }

    #[test]
    fn option_shape_remains_explicit() {
        let _ = Options {
            high_pass_hz: None,
            model_only_high_pass: false,
            model_gain: 1.0,
            passes: 1,
            model_file: None,
            enhanced: false,
            dereverb_strength: 0.35,
            strength: 1.0,
            adaptive: None,
            input_path: "in.wav".into(),
            output_path: "out.wav".into(),
        };
    }

    #[test]
    fn sixty_hz_lab_filter_matches_production_dc_blocker() {
        let mut laboratory = (0..4_937)
            .map(|index| ((index as f32 * 0.017).sin() * 0.4) + 0.1)
            .collect::<Vec<_>>();
        let mut production = laboratory.clone();
        apply_high_pass(&mut laboratory, 60.0);
        let mut blocker = DcBlocker::new();
        blocker.process(&mut production);
        assert_eq!(laboratory, production);
    }
}
