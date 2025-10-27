use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use opus::{Channels, Decoder};
use std::sync::Arc;
use tokio::sync::Mutex;
use webrtc::track::track_remote::TrackRemote;

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 960; // 20ms at 48kHz

pub struct AudioPlayback {
    #[allow(dead_code)]
    stream: Stream,
}

impl AudioPlayback {
    /// Start playing audio from a remote track
    pub fn start(
        track: Arc<TrackRemote>,
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Get the default audio host
        let host = cpal::default_host();

        // Get the default output device
        let device = host
            .default_output_device()
            .ok_or("No output device available")?;

        tracing::info!("Using audio output device: {}", device.name()?);

        // Create output config
        let config = StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };

        // Create shared buffer for decoded audio
        let audio_buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let audio_buffer_clone = Arc::clone(&audio_buffer);

        // Spawn task to receive and decode RTP packets using provided runtime
        runtime.spawn(async move {
            if let Err(e) = Self::audio_receive_task(track, audio_buffer_clone).await {
                tracing::error!("Audio receive task failed: {}", e);
            }
        });

        // Build the output stream
        let stream = match device.default_output_config()?.sample_format() {
            SampleFormat::F32 => Self::build_output_stream::<f32>(&device, &config, audio_buffer)?,
            SampleFormat::I16 => Self::build_output_stream::<i16>(&device, &config, audio_buffer)?,
            SampleFormat::U16 => Self::build_output_stream::<u16>(&device, &config, audio_buffer)?,
            format => return Err(format!("Unsupported sample format: {:?}", format).into()),
        };

        // Start the stream
        stream.play()?;

        Ok(Self { stream })
    }

    /// Build an output stream for a specific sample type
    fn build_output_stream<T>(
        device: &cpal::Device,
        config: &StreamConfig,
        audio_buffer: Arc<Mutex<Vec<f32>>>,
    ) -> Result<Stream, Box<dyn std::error::Error>>
    where
        T: cpal::Sample + cpal::SizedSample + cpal::FromSample<f32>,
    {
        let channels = config.channels as usize;

        let stream = device.build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                // Try to get audio samples from buffer
                if let Ok(mut buffer) = audio_buffer.try_lock() {
                    for frame in data.chunks_mut(channels) {
                        // Get next sample or use silence
                        let sample = if !buffer.is_empty() {
                            buffer.remove(0)
                        } else {
                            0.0 // Silence
                        };

                        // Write to all channels
                        for channel_sample in frame.iter_mut() {
                            *channel_sample = cpal::Sample::from_sample(sample);
                        }
                    }
                }
            },
            |err| {
                tracing::error!("Audio output stream error: {}", err);
            },
            None,
        )?;

        Ok(stream)
    }

    /// Task to receive and decode RTP packets
    async fn audio_receive_task(
        track: Arc<TrackRemote>,
        audio_buffer: Arc<Mutex<Vec<f32>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create Opus decoder
        let mut decoder = Decoder::new(SAMPLE_RATE, Channels::Mono)?;

        tracing::info!("Starting audio receive loop for track: {}", track.id());

        loop {
            // Read RTP packet
            let (rtp_packet, _) = match track.read_rtp().await {
                Ok(result) => result,
                Err(e) => {
                    tracing::debug!("RTP read error: {}", e);
                    break;
                }
            };

            // Decode Opus payload
            let mut decoded = vec![0i16; FRAME_SIZE];
            match decoder.decode(&rtp_packet.payload, &mut decoded, false) {
                Ok(size) => {
                    // Convert i16 to f32 and add to buffer
                    let samples_f32: Vec<f32> = decoded[..size]
                        .iter()
                        .map(|&s| s as f32 / 32767.0)
                        .collect();

                    let mut buffer = audio_buffer.lock().await;
                    buffer.extend(samples_f32);

                    // Prevent buffer from growing too large (keep max 1 second)
                    let buffer_len = buffer.len();
                    if buffer_len > SAMPLE_RATE as usize {
                        buffer.drain(0..(buffer_len - SAMPLE_RATE as usize));
                    }
                }
                Err(e) => {
                    tracing::debug!("Opus decode error: {}", e);
                }
            }
        }

        tracing::info!("Audio receive loop ended for track: {}", track.id());
        Ok(())
    }
}
