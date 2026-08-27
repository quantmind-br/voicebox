use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioOutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub struct AudioOutputState {
    host: Host,
    active_stop_flag: Mutex<Option<Arc<AtomicBool>>>,
}

impl AudioOutputState {
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
            active_stop_flag: Mutex::new(None),
        }
    }

    pub fn stop_all_playback(&self) -> Result<(), String> {
        let stop_flag = self
            .active_stop_flag
            .lock()
            .map_err(|_| "Audio playback state lock is poisoned".to_string())?
            .take();
        if let Some(stop_flag) = stop_flag {
            stop_flag.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn clear_stop_flag_if_current(&self, expected: &Arc<AtomicBool>) -> Result<(), String> {
        let mut active = self
            .active_stop_flag
            .lock()
            .map_err(|_| "Audio playback state lock is poisoned".to_string())?;
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            active.take();
        }
        Ok(())
    }

    pub fn list_output_devices(&self) -> Result<Vec<AudioOutputDevice>, String> {
        let devices = self
            .host
            .output_devices()
            .map_err(|e| format!("Failed to enumerate output devices: {}", e))?;

        let default_device = self.host.default_output_device();

        let mut result = Vec::new();
        for device in devices {
            let name = device
                .name()
                .map_err(|e| format!("Failed to get device name: {}", e))?;

            // Generate a stable ID from the device name (cpal doesn't provide stable IDs)
            let id = format!("device_{}", name.replace(' ', "_").to_lowercase());

            let is_default = default_device
                .as_ref()
                .map(|d| d.name().unwrap_or_default() == name)
                .unwrap_or(false);

            result.push(AudioOutputDevice {
                id,
                name,
                is_default,
            });
        }

        Ok(result)
    }

    pub async fn play_audio_to_devices(
        &self,
        audio_data: Vec<u8>,
        device_ids: Vec<String>,
    ) -> Result<(), String> {
        let (samples, sample_rate, channels) = self.decode_wav(&audio_data)?;
        let samples: Arc<[f32]> = samples.into();

        let devices: Vec<Device> = self
            .host
            .output_devices()
            .map_err(|e| format!("Failed to enumerate devices: {}", e))?
            .filter_map(|device| {
                let name = device.name().ok()?;
                let id = format!("device_{}", name.replace(' ', "_").to_lowercase());
                device_ids.contains(&id).then_some(device)
            })
            .collect();

        if devices.is_empty() {
            return Err("No matching devices found".to_string());
        }

        self.stop_all_playback()?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        *self
            .active_stop_flag
            .lock()
            .map_err(|_| "Audio playback state lock is poisoned".to_string())? =
            Some(stop_flag.clone());

        let mut ready_receivers = Vec::with_capacity(devices.len());
        for (index, device) in devices.into_iter().enumerate() {
            let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
            let thread_samples = samples.clone();
            let thread_stop_flag = stop_flag.clone();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);

            let spawn_result = std::thread::Builder::new()
                .name(format!("voicebox-audio-output-{index}"))
                .spawn(move || {
                    if let Err(error) = Self::play_to_device(
                        device,
                        thread_samples,
                        sample_rate,
                        channels,
                        thread_stop_flag,
                        ready_tx.clone(),
                    ) {
                        let _ = ready_tx.send(Err(error));
                    }
                });

            if let Err(error) = spawn_result {
                stop_flag.store(true, Ordering::Relaxed);
                self.clear_stop_flag_if_current(&stop_flag)?;
                return Err(format!(
                    "Failed to start playback thread for device {}: {}",
                    device_name, error
                ));
            }
            ready_receivers.push((device_name, ready_rx));
        }

        let readiness = tokio::task::spawn_blocking(move || {
            ready_receivers
                .into_iter()
                .map(|(device_name, receiver)| match receiver.recv() {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(format!(
                        "Failed to play to device {}: {}",
                        device_name, error
                    )),
                    Err(_) => Err(format!(
                        "Playback thread for device {} exited before reporting readiness",
                        device_name
                    )),
                })
                .collect::<Vec<Result<(), String>>>()
        })
        .await
        .map_err(|error| format!("Playback startup task failed: {}", error))?;

        let errors = readiness
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            stop_flag.store(true, Ordering::Relaxed);
            self.clear_stop_flag_if_current(&stop_flag)?;
            return Err(errors.join("; "));
        }

        Ok(())
    }

    fn decode_wav(&self, data: &[u8]) -> Result<(Vec<f32>, u32, u16), String> {
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;

        eprintln!(
            "decode_wav: Creating MediaSourceStream from {} bytes",
            data.len()
        );
        let mss = MediaSourceStream::new(
            Box::new(std::io::Cursor::new(data.to_vec())),
            Default::default(),
        );

        eprintln!("decode_wav: Probing audio format...");
        let mut format = symphonia::default::get_probe()
            .format(
                &Default::default(),
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| {
                eprintln!("decode_wav: Failed to probe audio: {}", e);
                format!("Failed to probe audio: {}", e)
            })?
            .format;

        eprintln!("decode_wav: Audio format probed successfully");

        eprintln!("decode_wav: Finding audio track...");
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or_else(|| {
                eprintln!("decode_wav: No audio track found");
                "No audio track found".to_string()
            })?;

        let sample_rate = track.codec_params.sample_rate.ok_or_else(|| {
            eprintln!("decode_wav: No sample rate found in track");
            "No sample rate found".to_string()
        })?;

        let channels = track
            .codec_params
            .channels
            .ok_or_else(|| {
                eprintln!("decode_wav: No channels found in track");
                "No channels found".to_string()
            })?
            .count() as u16;

        eprintln!(
            "decode_wav: Track info - sample_rate: {}, channels: {}",
            sample_rate, channels
        );

        eprintln!("decode_wav: Creating decoder...");
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &Default::default())
            .map_err(|e| {
                eprintln!("decode_wav: Failed to create decoder: {}", e);
                format!("Failed to create decoder: {}", e)
            })?;

        eprintln!("decode_wav: Decoder created successfully");

        let mut samples = Vec::new();
        let mut packet_count = 0;
        eprintln!("decode_wav: Starting packet decoding loop...");
        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(e) => {
                    eprintln!("decode_wav: End of stream or error: {:?}", e);
                    break;
                }
            };

            packet_count += 1;
            let decoded = decoder.decode(&packet).map_err(|e| {
                eprintln!("decode_wav: Decode error on packet {}: {}", packet_count, e);
                format!("Decode error: {}", e)
            })?;

            // Convert to f32 samples by matching on the buffer type
            use symphonia::core::audio::{AudioBufferRef, Signal};
            use symphonia::core::conv::FromSample;

            let spec = *decoded.spec();
            let num_channels = spec.channels.count();
            let num_frames = decoded.frames();

            // Interleave samples from all channels
            for frame_idx in 0..num_frames {
                for ch in 0..num_channels {
                    let sample_f32 = match &decoded {
                        AudioBufferRef::U8(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::U16(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::U24(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::U32(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::S8(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::S16(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::S24(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::S32(buf) => f32::from_sample(buf.chan(ch)[frame_idx]),
                        AudioBufferRef::F32(buf) => buf.chan(ch)[frame_idx],
                        AudioBufferRef::F64(buf) => buf.chan(ch)[frame_idx] as f32,
                    };
                    samples.push(sample_f32);
                }
            }
        }

        eprintln!(
            "decode_wav: Decoded {} packets, total {} samples",
            packet_count,
            samples.len()
        );
        eprintln!(
            "decode_wav: Returning sample_rate={}, channels={}",
            sample_rate, channels
        );
        Ok((samples, sample_rate, channels))
    }

    fn play_to_device(
        device: Device,
        samples: Arc<[f32]>,
        sample_rate: u32,
        channels: u16,
        stop_flag: Arc<AtomicBool>,
        ready_tx: mpsc::SyncSender<Result<(), String>>,
    ) -> Result<(), String> {
        let config = device
            .default_output_config()
            .map_err(|e| format!("Failed to get default config: {}", e))?;

        let device_sample_rate = config.sample_rate().0;
        let device_channels = config.channels();
        let resampled = (device_sample_rate != sample_rate)
            .then(|| Self::resample(&samples, sample_rate, device_sample_rate));
        let source_samples = resampled.as_deref().unwrap_or(&samples);
        let buffer: Arc<[f32]> = if device_channels != channels {
            Self::interleave_channels(source_samples, channels, device_channels).into()
        } else if let Some(resampled) = resampled {
            resampled.into()
        } else {
            samples
        };

        let position = Arc::new(AtomicUsize::new(0));
        let err_fn = |err| eprintln!("Playback error: {}", err);
        let stream_config = StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let stream = match config.sample_format() {
            SampleFormat::F32 => {
                let buffer = buffer.clone();
                let position = position.clone();
                let stop_flag = stop_flag.clone();
                device
                    .build_output_stream(
                        &stream_config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            if stop_flag.load(Ordering::Relaxed) {
                                data.fill(0.0);
                                return;
                            }

                            let mut index = position.load(Ordering::Relaxed);
                            for sample in data.iter_mut() {
                                if let Some(value) = buffer.get(index) {
                                    *sample = *value;
                                    index += 1;
                                } else {
                                    *sample = 0.0;
                                }
                            }
                            position.store(index, Ordering::Relaxed);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("Failed to build stream: {}", e))?
            }
            SampleFormat::I16 => {
                let buffer = buffer.clone();
                let position = position.clone();
                let stop_flag = stop_flag.clone();
                device
                    .build_output_stream(
                        &stream_config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            if stop_flag.load(Ordering::Relaxed) {
                                data.fill(0);
                                return;
                            }

                            let mut index = position.load(Ordering::Relaxed);
                            for sample in data.iter_mut() {
                                if let Some(value) = buffer.get(index) {
                                    *sample = (*value * 32767.0) as i16;
                                    index += 1;
                                } else {
                                    *sample = 0;
                                }
                            }
                            position.store(index, Ordering::Relaxed);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("Failed to build stream: {}", e))?
            }
            SampleFormat::U16 => {
                let buffer = buffer.clone();
                let position = position.clone();
                let stop_flag = stop_flag.clone();
                device
                    .build_output_stream(
                        &stream_config,
                        move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                            if stop_flag.load(Ordering::Relaxed) {
                                data.fill(32768);
                                return;
                            }

                            let mut index = position.load(Ordering::Relaxed);
                            for sample in data.iter_mut() {
                                if let Some(value) = buffer.get(index) {
                                    *sample = ((*value + 1.0) * 32767.5) as u16;
                                    index += 1;
                                } else {
                                    *sample = 32768;
                                }
                            }
                            position.store(index, Ordering::Relaxed);
                        },
                        err_fn,
                        None,
                    )
                    .map_err(|e| format!("Failed to build stream: {}", e))?
            }
            _ => return Err("Unsupported sample format".to_string()),
        };

        stream
            .play()
            .map_err(|e| format!("Failed to play stream: {}", e))?;
        let _ = ready_tx.send(Ok(()));

        let total_samples = buffer.len();
        while position.load(Ordering::Relaxed) < total_samples && !stop_flag.load(Ordering::Relaxed)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        drop(stream);
        Ok(())
    }

    fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
        if from_rate == to_rate {
            return samples.to_vec();
        }

        let ratio = to_rate as f64 / from_rate as f64;
        let new_len = (samples.len() as f64 * ratio) as usize;
        let mut resampled = Vec::with_capacity(new_len);

        for i in 0..new_len {
            let src_idx = (i as f64 / ratio) as usize;
            if src_idx < samples.len() {
                resampled.push(samples[src_idx]);
            } else {
                resampled.push(0.0);
            }
        }

        resampled
    }

    fn interleave_channels(samples: &[f32], src_channels: u16, dst_channels: u16) -> Vec<f32> {
        if src_channels == dst_channels {
            return samples.to_vec();
        }

        let mut interleaved = Vec::new();
        let samples_per_channel = samples.len() / src_channels as usize;

        for i in 0..samples_per_channel {
            for ch in 0..dst_channels {
                let src_ch = if ch < src_channels {
                    ch
                } else {
                    src_channels - 1
                };
                let idx = (i * src_channels as usize) + src_ch as usize;
                if idx < samples.len() {
                    interleaved.push(samples[idx]);
                } else {
                    interleaved.push(0.0);
                }
            }
        }

        interleaved
    }
}

impl Default for AudioOutputState {
    fn default() -> Self {
        Self::new()
    }
}
