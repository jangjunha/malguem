use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use opus::{Application, Channels, Encoder};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::api::media_engine::MIME_TYPE_OPUS;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

const SAMPLE_RATE: u32 = 48000; // Opus works best at 48kHz
const FRAME_SIZE: usize = 960; // 20ms at 48kHz (48000 * 0.02)

pub struct AudioCapture {
    #[allow(dead_code)]
    stream: Stream,
    track: Arc<TrackLocalStaticSample>,
}

impl AudioCapture {
    /// Start capturing audio from the default microphone
    pub fn start(
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Get the default audio host
        let host = cpal::default_host();

        // Get the default input device
        let device = host
            .default_input_device()
            .ok_or("No input device available")?;

        tracing::info!("Using audio input device: {}", device.name()?);

        // Get the default input config
        let config = device.default_input_config()?;
        tracing::info!("Default input config: {:?}", config);

        // Create WebRTC audio track
        let track: Arc<TrackLocalStaticSample> = Arc::new(TrackLocalStaticSample::new(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_string(),
                clock_rate: 48000,
                channels: 1,
                ..Default::default()
            },
            "audio".to_string(),
            "webrtc-rs".to_string(),
        ));

        // Create audio processing channel
        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<Vec<f32>>();

        // Spawn audio processing task using provided runtime
        let track_clone = Arc::clone(&track);
        runtime.spawn(async move {
            if let Err(e) = Self::audio_processing_task(track_clone, &mut audio_rx).await {
                tracing::error!("Audio processing task failed: {}", e);
            }
        });

        // Build the input stream
        let stream = match config.sample_format() {
            SampleFormat::F32 => {
                Self::build_input_stream::<f32>(&device, &config.into(), audio_tx)?
            }
            SampleFormat::I16 => {
                Self::build_input_stream::<i16>(&device, &config.into(), audio_tx)?
            }
            SampleFormat::U16 => {
                Self::build_input_stream::<u16>(&device, &config.into(), audio_tx)?
            }
            format => return Err(format!("Unsupported sample format: {:?}", format).into()),
        };

        // Start the stream
        stream.play()?;

        Ok(Self { stream, track })
    }

    /// Build an input stream for a specific sample type
    fn build_input_stream<T>(
        device: &Device,
        config: &StreamConfig,
        audio_tx: mpsc::UnboundedSender<Vec<f32>>,
    ) -> Result<Stream, Box<dyn std::error::Error>>
    where
        T: cpal::Sample + cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let channels = config.channels as usize;
        let mut buffer: Vec<f32> = Vec::new();

        let stream = device.build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // Convert samples to f32 and accumulate
                for frame in data.chunks(channels) {
                    // Take only the first channel (mono)
                    if let Some(&sample) = frame.first() {
                        let f32_sample: f32 = cpal::Sample::from_sample(sample);
                        buffer.push(f32_sample);

                        // When we have enough samples for a frame, send it
                        if buffer.len() >= FRAME_SIZE {
                            let frame_data: Vec<f32> = buffer.drain(..FRAME_SIZE).collect();
                            let _ = audio_tx.send(frame_data);
                        }
                    }
                }
            },
            |err| {
                tracing::error!("Audio stream error: {}", err);
            },
            None,
        )?;

        Ok(stream)
    }

    /// Audio processing task that encodes and sends audio
    async fn audio_processing_task(
        track: Arc<TrackLocalStaticSample>,
        audio_rx: &mut mpsc::UnboundedReceiver<Vec<f32>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create Opus encoder
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;

        // Set bitrate to 32kbps (good for voice)
        encoder.set_bitrate(opus::Bitrate::Bits(32000))?;

        let mut sequence_number: u16 = 0;
        let mut timestamp: u32 = 0;

        while let Some(audio_frame) = audio_rx.recv().await {
            // Convert f32 samples to i16 for Opus
            let samples_i16: Vec<i16> = audio_frame
                .iter()
                .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
                .collect();

            // Encode with Opus
            let mut encoded = vec![0u8; 4000]; // Max Opus frame size
            match encoder.encode(&samples_i16, &mut encoded) {
                Ok(size) => {
                    encoded.truncate(size);

                    // Write RTP packet
                    if let Err(e) = track
                        .write_sample(&Sample {
                            data: Bytes::from(encoded),
                            duration: Duration::from_secs_f64(
                                FRAME_SIZE as f64 / SAMPLE_RATE as f64,
                            ),
                            ..Default::default()
                        })
                        .await
                    {
                        tracing::error!("Failed to write RTP packet: {}", e);
                    }

                    sequence_number = sequence_number.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(FRAME_SIZE as u32);
                }
                Err(e) => {
                    tracing::error!("Opus encoding failed: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Get the audio track for adding to peer connections
    pub fn track(&self) -> Arc<TrackLocalStaticSample> {
        Arc::clone(&self.track)
    }
}
