//! Deterministic feature generation for versioned `RNNoise` training candidates.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use hound::WavReader;
use nnnoiseless::{
    DenoiseFeatures, EBAND_5MS, FRAME_SIZE, FRAME_SIZE_SHIFT, FREQ_SIZE, NB_BANDS, NB_FEATURES,
};

const ROW_VALUES: usize = NB_FEATURES + 2 * NB_BANDS + 1;
const AUGMENTATION_FRAMES: usize = 200;

fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse(env::args().skip(1))?;
    let pairs = paired_wavs(
        &options.clean_dir,
        &options.noisy_dir,
        &options.include_speakers,
        &options.exclude_speakers,
        options.residual_scales.as_deref(),
    )?;
    if pairs.is_empty() {
        return Err("no paired WAV files were found".into());
    }

    let output = File::create(&options.output)?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, output);
    let mut generator = FeatureGenerator::new(pairs, options.seed)?;
    for frame_index in 0..options.frames {
        let row = generator.next_row()?;
        for value in row {
            writer.write_all(&value.to_le_bytes())?;
        }
        if frame_index > 0 && frame_index % 10_000 == 0 {
            eprintln!("generated {frame_index}/{} frames", options.frames);
        }
    }
    writer.flush()?;
    eprintln!(
        "generated {} rows x {ROW_VALUES} values from {} paired files",
        options.frames,
        generator.pair_count()
    );
    Ok(())
}

struct Options {
    clean_dir: PathBuf,
    noisy_dir: PathBuf,
    output: PathBuf,
    frames: usize,
    seed: u64,
    include_speakers: Vec<String>,
    exclude_speakers: Vec<String>,
    residual_scales: Option<PathBuf>,
}

impl Options {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, Box<dyn Error>> {
        let mut clean_dir = None;
        let mut noisy_dir = None;
        let mut output = None;
        let mut frames = None;
        let mut seed = 0x4e4f_4952_4552_4e4e_u64;
        let mut include_speakers = Vec::new();
        let mut exclude_speakers = Vec::new();
        let mut residual_scales = None;
        let mut arguments = arguments;
        while let Some(argument) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value after {argument}"))?;
            match argument.as_str() {
                "--clean-dir" => clean_dir = Some(PathBuf::from(value)),
                "--noisy-dir" => noisy_dir = Some(PathBuf::from(value)),
                "--output" => output = Some(PathBuf::from(value)),
                "--frames" => frames = Some(value.parse::<usize>()?),
                "--seed" => seed = value.parse::<u64>()?,
                "--include-speakers" => include_speakers = speaker_list(&value),
                "--exclude-speakers" => exclude_speakers = speaker_list(&value),
                "--residual-scales" => residual_scales = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown argument: {argument}").into()),
            }
        }
        let frames = frames.ok_or("--frames is required")?;
        if frames == 0 {
            return Err("--frames must be greater than zero".into());
        }
        if seed == 0 {
            return Err("--seed must be non-zero".into());
        }
        if !include_speakers.is_empty() && !exclude_speakers.is_empty() {
            return Err("speaker include and exclude filters cannot be combined".into());
        }
        Ok(Self {
            clean_dir: clean_dir.ok_or("--clean-dir is required")?,
            noisy_dir: noisy_dir.ok_or("--noisy-dir is required")?,
            output: output.ok_or("--output is required")?,
            frames,
            seed,
            include_speakers,
            exclude_speakers,
            residual_scales,
        })
    }
}

fn speaker_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|speaker| !speaker.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn paired_wavs(
    clean_dir: &Path,
    noisy_dir: &Path,
    include_speakers: &[String],
    exclude_speakers: &[String],
    residual_scales_path: Option<&Path>,
) -> Result<Vec<AudioPair>, Box<dyn Error>> {
    let residual_scales = residual_scales_path.map(read_residual_scales).transpose()?;
    let mut result = Vec::new();
    for entry in fs::read_dir(clean_dir)? {
        let clean = entry?.path();
        let is_wav = clean
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"));
        if !is_wav {
            continue;
        }
        let Some(name) = clean.file_name() else {
            continue;
        };
        let name_text = name.to_string_lossy();
        let speaker = name_text.split('_').next().unwrap_or_default();
        if (!include_speakers.is_empty() && !include_speakers.iter().any(|value| value == speaker))
            || exclude_speakers.iter().any(|value| value == speaker)
        {
            continue;
        }
        let noisy = noisy_dir.join(name);
        if noisy.is_file() {
            let residual_clean_scale = if let Some(scales) = &residual_scales {
                *scales
                    .get(
                        clean
                            .file_stem()
                            .and_then(|value| value.to_str())
                            .unwrap_or_default(),
                    )
                    .ok_or_else(|| format!("missing residual scale for {}", clean.display()))?
            } else {
                1.0
            };
            result.push(AudioPair {
                clean,
                noisy,
                residual_clean_scale,
            });
        }
    }
    result.sort_by(|left, right| left.clean.cmp(&right.clean));
    Ok(result)
}

fn read_residual_scales(
    path: &Path,
) -> Result<std::collections::HashMap<String, f32>, Box<dyn Error>> {
    let mut result = std::collections::HashMap::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let mut fields = line.split_whitespace();
        let case_id = fields
            .next()
            .ok_or("missing case ID in residual scale file")?;
        let scale = fields
            .next()
            .ok_or("missing value in residual scale file")?
            .parse::<f32>()?;
        if fields.next().is_some() || !scale.is_finite() || !(0.125..=2.0).contains(&scale) {
            return Err(format!("invalid residual scale line: {line}").into());
        }
        if result.insert(case_id.to_owned(), scale).is_some() {
            return Err(format!("duplicate residual scale for {case_id}").into());
        }
    }
    if result.is_empty() {
        return Err("residual scale file is empty".into());
    }
    Ok(result)
}

#[derive(Clone)]
struct AudioPair {
    clean: PathBuf,
    noisy: PathBuf,
    residual_clean_scale: f32,
}

struct FeatureGenerator {
    speech: WavStream,
    residual: ResidualStream,
    clean_features: DenoiseFeatures,
    noise_features: DenoiseFeatures,
    mixture_features: DenoiseFeatures,
    rng: DeterministicRng,
    augmentation: Augmentation,
    augmentation_left: usize,
    pair_count: usize,
    vad_count: i32,
    clean: [f32; FRAME_SIZE],
    noise: [f32; FRAME_SIZE],
    mixture: [f32; FRAME_SIZE],
    clean_filter_memory: [f32; 2],
    noise_filter_memory: [f32; 2],
}

impl FeatureGenerator {
    fn new(pairs: Vec<AudioPair>, seed: u64) -> Result<Self, Box<dyn Error>> {
        let pair_count = pairs.len();
        let mut speech_paths: Vec<PathBuf> = pairs.iter().map(|pair| pair.clean.clone()).collect();
        let mut residual_pairs = pairs;
        let mut rng = DeterministicRng::new(seed);
        rng.shuffle(&mut speech_paths);
        rng.shuffle(&mut residual_pairs);
        Ok(Self {
            speech: WavStream::new(speech_paths)?,
            residual: ResidualStream::new(residual_pairs)?,
            clean_features: DenoiseFeatures::new(),
            noise_features: DenoiseFeatures::new(),
            mixture_features: DenoiseFeatures::new(),
            rng,
            augmentation: Augmentation::default(),
            augmentation_left: 0,
            pair_count,
            vad_count: 0,
            clean: [0.0; FRAME_SIZE],
            noise: [0.0; FRAME_SIZE],
            mixture: [0.0; FRAME_SIZE],
            clean_filter_memory: [0.0; 2],
            noise_filter_memory: [0.0; 2],
        })
    }

    const fn pair_count(&self) -> usize {
        self.pair_count
    }

    fn next_row(&mut self) -> Result<[f32; ROW_VALUES], Box<dyn Error>> {
        if self.augmentation_left == 0 {
            self.augmentation = Augmentation::random(&mut self.rng);
            self.augmentation_left = AUGMENTATION_FRAMES;
            self.clean_filter_memory = [0.0; 2];
            self.noise_filter_memory = [0.0; 2];
        }
        self.augmentation_left -= 1;

        self.speech.read_frame(&mut self.clean)?;
        self.residual.read_frame(&mut self.noise)?;
        let original_energy = self.clean.iter().map(|sample| sample * sample).sum::<f32>();
        self.augmentation
            .clean_filter
            .filter_in_place(&mut self.clean, &mut self.clean_filter_memory);
        self.augmentation
            .noise_filter
            .filter_in_place(&mut self.noise, &mut self.noise_filter_memory);
        for sample in &mut self.clean {
            *sample *= self.augmentation.clean_gain;
        }
        for sample in &mut self.noise {
            *sample *= self.augmentation.noise_gain;
        }
        for ((mixture, clean), noise) in self.mixture.iter_mut().zip(&self.clean).zip(&self.noise) {
            *mixture = clean + noise;
        }
        let peak = self
            .mixture
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        if peak > 30_000.0 {
            let scale = 30_000.0 / peak;
            for sample in &mut self.clean {
                *sample *= scale;
            }
            for sample in &mut self.noise {
                *sample *= scale;
            }
            for sample in &mut self.mixture {
                *sample *= scale;
            }
        }

        self.clean_features.shift_and_filter_input(&self.clean);
        self.noise_features.shift_and_filter_input(&self.noise);
        self.mixture_features.shift_and_filter_input(&self.mixture);
        self.clean_features.compute_frame_features();
        self.noise_features.compute_frame_features();
        let silence = self.mixture_features.compute_frame_features();

        let vad = self.vad(original_energy);
        let band_gain_cutoff = if silence {
            0
        } else {
            self.augmentation.band_cutoff
        };
        let mut row = [0.0_f32; ROW_VALUES];
        row[..NB_FEATURES].copy_from_slice(self.mixture_features.features());
        for (index, gain) in row[NB_FEATURES..NB_FEATURES + NB_BANDS]
            .iter_mut()
            .enumerate()
        {
            *gain = if index >= band_gain_cutoff
                || (self.clean_features.ex[index] < 5.0e-2
                    && self.mixture_features.ex[index] < 5.0e-2)
            {
                -1.0
            } else {
                ((self.clean_features.ex[index] + 1.0e-3)
                    / (self.mixture_features.ex[index] + 1.0e-3))
                    .sqrt()
                    .min(1.0)
            };
        }
        let noise_offset = NB_FEATURES + NB_BANDS;
        for (level, energy) in row[noise_offset..noise_offset + NB_BANDS]
            .iter_mut()
            .zip(self.noise_features.ex)
        {
            *level = (energy + 1.0e-2).log10();
        }
        row[ROW_VALUES - 1] = vad;
        Ok(row)
    }

    fn vad(&mut self, clean_energy: f32) -> f32 {
        if self.augmentation.clean_gain == 0.0 {
            self.vad_count = 15;
        } else if clean_energy > 1.0e9 {
            self.vad_count = 0;
        } else if clean_energy > 1.0e8 {
            self.vad_count -= 5;
        } else if clean_energy > 1.0e7 {
            self.vad_count += 1;
        } else {
            self.vad_count += 2;
        }
        self.vad_count = self.vad_count.clamp(0, 15);
        if self.vad_count >= 10 {
            0.0
        } else if self.vad_count > 0 {
            0.5
        } else {
            1.0
        }
    }
}

struct WavStream {
    paths: Vec<PathBuf>,
    next_path: usize,
    reader: Option<WavReader<BufReader<File>>>,
    remaining_samples: usize,
}

impl WavStream {
    fn new(paths: Vec<PathBuf>) -> Result<Self, Box<dyn Error>> {
        if paths.is_empty() {
            return Err("audio stream requires at least one file".into());
        }
        Ok(Self {
            paths,
            next_path: 0,
            reader: None,
            remaining_samples: 0,
        })
    }

    fn open_next(&mut self) -> Result<(), Box<dyn Error>> {
        for _ in 0..self.paths.len() {
            let path = &self.paths[self.next_path];
            self.next_path = (self.next_path + 1) % self.paths.len();
            let reader = WavReader::open(path)?;
            validate_wav(reader.spec(), path)?;
            let remaining_samples = reader.duration() as usize;
            if remaining_samples >= FRAME_SIZE {
                self.reader = Some(reader);
                self.remaining_samples = remaining_samples;
                return Ok(());
            }
        }
        Err("none of the speech WAVs contains one complete frame".into())
    }

    fn read_frame(&mut self, output: &mut [f32; FRAME_SIZE]) -> Result<(), Box<dyn Error>> {
        if self.reader.is_none() || self.remaining_samples < FRAME_SIZE {
            self.open_next()?;
        }
        let Some(reader) = self.reader.as_mut() else {
            return Err("audio reader was not initialized".into());
        };
        for (destination, sample) in output.iter_mut().zip(reader.samples::<i16>()) {
            *destination = f32::from(sample?);
        }
        self.remaining_samples -= FRAME_SIZE;
        if self.remaining_samples < FRAME_SIZE {
            self.reader = None;
        }
        Ok(())
    }
}

struct ResidualStream {
    pairs: Vec<AudioPair>,
    next_pair: usize,
    clean: Option<WavReader<BufReader<File>>>,
    noisy: Option<WavReader<BufReader<File>>>,
    remaining_samples: usize,
    clean_scale: f32,
}

impl ResidualStream {
    fn new(pairs: Vec<AudioPair>) -> Result<Self, Box<dyn Error>> {
        if pairs.is_empty() {
            return Err("residual stream requires at least one pair".into());
        }
        Ok(Self {
            pairs,
            next_pair: 0,
            clean: None,
            noisy: None,
            remaining_samples: 0,
            clean_scale: 1.0,
        })
    }

    fn open_next(&mut self) -> Result<(), Box<dyn Error>> {
        for _ in 0..self.pairs.len() {
            let pair = &self.pairs[self.next_pair];
            self.next_pair = (self.next_pair + 1) % self.pairs.len();
            let clean = WavReader::open(&pair.clean)?;
            let noisy = WavReader::open(&pair.noisy)?;
            validate_wav(clean.spec(), &pair.clean)?;
            validate_wav(noisy.spec(), &pair.noisy)?;
            if clean.duration() != noisy.duration() {
                return Err(format!("paired WAV length mismatch: {}", pair.clean.display()).into());
            }
            let remaining_samples = clean.duration() as usize;
            if remaining_samples >= FRAME_SIZE {
                self.clean = Some(clean);
                self.noisy = Some(noisy);
                self.remaining_samples = remaining_samples;
                self.clean_scale = pair.residual_clean_scale;
                return Ok(());
            }
        }
        Err("none of the residual WAV pairs contains one complete frame".into())
    }

    fn read_frame(&mut self, output: &mut [f32; FRAME_SIZE]) -> Result<(), Box<dyn Error>> {
        if self.clean.is_none() || self.noisy.is_none() || self.remaining_samples < FRAME_SIZE {
            self.open_next()?;
        }
        let (Some(clean), Some(noisy)) = (self.clean.as_mut(), self.noisy.as_mut()) else {
            return Err("residual readers were not initialized".into());
        };
        for ((destination, clean_sample), noisy_sample) in output
            .iter_mut()
            .zip(clean.samples::<i16>())
            .zip(noisy.samples::<i16>())
        {
            let clean_value = f32::from(clean_sample?);
            let noisy_value = f32::from(noisy_sample?);
            *destination = noisy_value - self.clean_scale * clean_value;
        }
        self.remaining_samples -= FRAME_SIZE;
        if self.remaining_samples < FRAME_SIZE {
            self.clean = None;
            self.noisy = None;
        }
        Ok(())
    }
}

fn validate_wav(spec: hound::WavSpec, path: &Path) -> Result<(), Box<dyn Error>> {
    if spec.channels != 1
        || spec.sample_rate != 48_000
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(format!("expected mono 48 kHz PCM16 WAV: {}", path.display()).into());
    }
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    a: [f32; 2],
    b: [f32; 2],
}

impl Biquad {
    fn filter_in_place(self, samples: &mut [f32], memory: &mut [f32; 2]) {
        for sample in samples {
            let input = f64::from(*sample);
            let output = input + f64::from(memory[0]);
            memory[0] = (f64::from(memory[1]) + f64::from(self.b[0]) * input
                - f64::from(self.a[0]) * output) as f32;
            memory[1] = (f64::from(self.b[1]) * input - f64::from(self.a[1]) * output) as f32;
            *sample = output as f32;
        }
    }
}

#[derive(Clone, Copy)]
struct Augmentation {
    clean_gain: f32,
    noise_gain: f32,
    clean_filter: Biquad,
    noise_filter: Biquad,
    band_cutoff: usize,
}

impl Default for Augmentation {
    fn default() -> Self {
        Self {
            clean_gain: 1.0,
            noise_gain: 1.0,
            clean_filter: Biquad::default(),
            noise_filter: Biquad::default(),
            band_cutoff: NB_BANDS,
        }
    }
}

impl Augmentation {
    fn random(rng: &mut DeterministicRng) -> Self {
        let mode = rng.range(100);
        let clean_gain = if (20..30).contains(&mode) {
            0.0
        } else {
            decibels(rng.uniform(-12.0, 6.0))
        };
        let noise_gain = match mode {
            0..20 => 0.0,
            20..30 => decibels(rng.uniform(-6.0, 6.0)),
            30..50 => decibels(rng.uniform(-18.0, -8.0)),
            50..75 => decibels(rng.uniform(-8.0, 2.0)),
            _ => decibels(rng.uniform(2.0, 12.0)),
        };
        let lowpass = (FREQ_SIZE as f32 * 3_000.0 / 24_000.0 * 50.0_f32.powf(rng.unit())) as usize;
        let band_cutoff = EBAND_5MS
            .iter()
            .position(|value| value << FRAME_SIZE_SHIFT > lowpass)
            .unwrap_or(NB_BANDS - 1)
            + 1;
        Self {
            clean_gain,
            noise_gain,
            clean_filter: random_filter(rng),
            noise_filter: random_filter(rng),
            band_cutoff,
        }
    }
}

fn random_filter(rng: &mut DeterministicRng) -> Biquad {
    let mut coefficient = || 0.75 * (rng.unit() - 0.5);
    Biquad {
        a: [coefficient(), coefficient()],
        b: [coefficient(), coefficient()],
    }
}

fn decibels(value: f32) -> f32 {
    10.0_f32.powf(value / 20.0)
}

struct DeterministicRng(u64);

impl DeterministicRng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn unit(&mut self) -> f32 {
        let mantissa = u32::try_from(self.next() >> 40).unwrap_or_default();
        mantissa as f32 / 16_777_216.0
    }

    fn uniform(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }

    fn range(&mut self, upper: usize) -> usize {
        usize::try_from(self.next() % upper as u64).unwrap_or_default()
    }

    fn shuffle<T>(&mut self, values: &mut [T]) {
        for index in (1..values.len()).rev() {
            values.swap(index, self.range(index + 1));
        }
    }
}
