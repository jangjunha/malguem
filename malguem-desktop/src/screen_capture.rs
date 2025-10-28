use bytes::Bytes;
use openh264::encoder::Encoder;
use openh264::formats::YUVBuffer;
use scap::capturer::{Capturer, Options};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use yuv::{
    YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    bgra_to_yuv420,
};

const TARGET_FPS: u32 = 30;
const FRAME_INTERVAL_MS: u64 = 1000 / TARGET_FPS as u64;
const H264_CLOCK_RATE: u32 = 90000;

struct FrameData {
    data: Vec<u8>,
    width: u32,
    height: u32,
}
pub struct ScreenCapture {
    stop_tx: mpsc::UnboundedSender<()>,
    track: Arc<TrackLocalStaticSample>,
}

impl ScreenCapture {
    /// Start capturing screen
    pub fn start(
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        tracing::info!("Starting screen capture");
        if !scap::has_permission() {
            tracing::warn!("Screen capture permission not granted, requesting permission...");
            if !scap::request_permission() {
                return Err("Screen capture permission denied. Please grant permission in System Settings > Privacy & Security > Screen Recording".into());
            }
            tracing::info!("Screen capture permission granted");
        }

        // Create WebRTC video track for H.264
        let track = Arc::new(TrackLocalStaticSample::new(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_string(),
                clock_rate: H264_CLOCK_RATE,
                // sdp_fmtp_line:
                //     "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                //         .to_string(),
                ..Default::default()
            },
            "video".to_string(),
            "webrtc-rs".to_string(),
        ));

        // Create channels for frame processing and control
        let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<FrameData>();
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<()>();

        // Spawn capture task in a blocking thread (scap is synchronous)
        let stop_rx_clone = stop_tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = Self::capture_task(frame_tx, stop_rx_clone) {
                tracing::error!("Screen capture task failed: {}", e);
            }
        });

        // Spawn frame processing task using provided runtime
        let track_clone = Arc::clone(&track);
        runtime.spawn(async move {
            if let Err(e) =
                Self::frame_processing_task(track_clone, &mut frame_rx, &mut stop_rx).await
            {
                tracing::error!("Frame processing task failed: {}", e);
            }
        });

        Ok(Self { stop_tx, track })
    }

    /// Screen capture task (runs in blocking thread)
    fn capture_task(
        frame_tx: mpsc::UnboundedSender<FrameData>,
        stop_tx: mpsc::UnboundedSender<()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Get the main display
        let display = scap::get_main_display();
        tracing::info!("Capturing main display");

        // Create capturer options
        let options = Options {
            target: Some(scap::Target::Display(display)),
            show_cursor: true,
            show_highlight: false,
            ..Default::default()
        };

        // Create capturer
        let mut capturer = Capturer::build(options)?;

        // Start capturing frames
        capturer.start_capture();

        loop {
            // Check if we should stop (non-blocking check)
            if stop_tx.is_closed() {
                break;
            }

            // Capture a frame
            match capturer.get_next_frame() {
                Ok(frame) => {
                    // Extract frame data based on Frame enum variant
                    use scap::frame::Frame;

                    let (width, height, bgra_data) = match frame {
                        Frame::Video(video_frame) => match video_frame {
                            scap::frame::VideoFrame::BGRA(bgra_frame) => {
                                // Access BGRA frame data
                                (
                                    bgra_frame.width as u32,
                                    bgra_frame.height as u32,
                                    bgra_frame.data.clone(),
                                )
                            }
                            scap::frame::VideoFrame::BGR0(bgr_frame) => {
                                // BGR0 format - treat as BGRA
                                (
                                    bgr_frame.width as u32,
                                    bgr_frame.height as u32,
                                    bgr_frame.data.clone(),
                                )
                            }
                            _ => {
                                tracing::warn!("Unsupported video frame format");
                                continue;
                            }
                        },
                        Frame::Audio(_) => {
                            tracing::warn!("Received audio frame instead of video");
                            continue;
                        }
                    };

                    // Send frame data for encoding
                    let frame_data = FrameData {
                        data: bgra_data,
                        width,
                        height,
                    };

                    if frame_tx.send(frame_data).is_err() {
                        tracing::warn!("Failed to send frame, receiver dropped");
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to capture frame: {}", e);
                }
            }

            // Sleep to maintain target FPS
            std::thread::sleep(std::time::Duration::from_millis(FRAME_INTERVAL_MS));
        }

        capturer.stop_capture();
        tracing::info!("Screen capture stopped");

        Ok(())
    }

    /// Frame processing task that encodes and sends video frames
    async fn frame_processing_task(
        track: Arc<TrackLocalStaticSample>,
        frame_rx: &mut mpsc::UnboundedReceiver<FrameData>,
        stop_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // H.264 encoder (created on first frame)
        let mut encoder: Option<Encoder> = None;
        let mut current_width: u32 = 0;
        let mut current_height: u32 = 0;

        loop {
            tokio::select! {
                Some(frame_data) = frame_rx.recv() => {
                    // Create encoder on first frame (encoder auto-reinitializes on resolution change)
                    if encoder.is_none() {
                        tracing::info!("Initializing H.264 encoder");
                        encoder = Some(Encoder::new()?);
                    }

                    if let Some(ref mut enc) = encoder {
                        // Track dimension changes
                        if frame_data.width != current_width || frame_data.height != current_height {
                            tracing::info!("Encoding {}x{} frames", frame_data.width, frame_data.height);
                            current_width = frame_data.width;
                            current_height = frame_data.height;
                        }

                        // Convert BGRA to YUV420
                        let yuv = Self::bgra_to_yuv420_buffer(
                            &frame_data.data,
                            frame_data.width,
                            frame_data.height
                        )?;

                        // Encode frame with H.264 and collect all NAL units
                        let nal_data = match enc.encode(&yuv) {
                            Ok(bitstream) => {
                                // Combine all NAL units from all layers into a single buffer
                                let mut combined = Vec::new();
                                for i in 0..bitstream.num_layers() {
                                    if let Some(layer) = bitstream.layer(i) {
                                        for j in 0..layer.nal_count() {
                                            if let Some(nal) = layer.nal_unit(j) {
                                                combined.extend_from_slice(nal);
                                            }
                                        }
                                    }
                                }
                                combined
                            }
                            Err(e) => {
                                tracing::error!("H.264 encoding failed: {}", e);
                                Vec::new()
                            }
                        };

                        if !nal_data.is_empty() {
                            if let Err(e) = track.write_sample(&Sample {
                                    data: Bytes::from(nal_data),
                                    duration: Duration::from_millis(FRAME_INTERVAL_MS),
                                    ..Default::default()
                                })
                                .await {
                                    tracing::error!("Failed to write RTP packet: {}", e);
                            }
                        }
                    }
                }
                Some(_) = stop_rx.recv() => {
                    tracing::info!("Stop signal received, ending frame processing");
                    break;
                }
            }
        }

        Ok(())
    }

    fn bgra_to_yuv420_buffer(
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> Result<YUVBuffer, Box<dyn std::error::Error>> {
        let yuv_image = {
            let mut yuv_image =
                YuvPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
            bgra_to_yuv420(
                &mut yuv_image,
                bgra,
                width,
                YuvRange::Limited,
                YuvStandardMatrix::Bt601,
                YuvConversionMode::default(),
            )?;
            yuv_image
        };
        Ok(YUVBuffer::from_vec(
            [
                yuv_image.y_plane.borrow(),
                yuv_image.u_plane.borrow(),
                yuv_image.v_plane.borrow(),
            ]
            .concat(),
            width as usize,
            height as usize,
        ))
    }

    /// Get the video track for adding to peer connections
    pub fn track(&self) -> Arc<TrackLocalStaticSample> {
        Arc::clone(&self.track)
    }

    /// Stop screen capture
    pub fn stop(&self) {
        let _ = self.stop_tx.send(());
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
