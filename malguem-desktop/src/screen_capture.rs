use bytes::Bytes;
use openh264::encoder::Encoder;
use openh264::formats::YUVBuffer;
use scap::capturer::{Capturer, Options};
use scap::frame::Frame;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::mpsc;
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use yuv::{
    BufferStoreMut, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    bgra_to_yuv420,
};

const TARGET_FPS: u32 = 30;
const FRAME_INTERVAL_MS: u64 = 1000 / TARGET_FPS as u64;
const H264_CLOCK_RATE: u32 = 90000;

pub struct FrameData {
    pub data: YUVBuffer,
    pub timestamp: SystemTime,
}

pub struct ScreenCapture {
    stop_tx: mpsc::UnboundedSender<()>,
    track: Arc<TrackLocalStaticSample>,
}

impl ScreenCapture {
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

        let track = Arc::new(TrackLocalStaticSample::new(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_string(),
                clock_rate: H264_CLOCK_RATE,
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
        tracing::info!("capture_task()");

        // Create capturer options
        let options = Options {
            fps: 30,
            target: None, // None captures the primary display
            output_type: scap::frame::FrameType::BGRAFrame,
            // output_resolution: scap::capturer::Resolution::_720p,
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
                    let frame_data = match frame {
                        Frame::Video(video_frame) => match video_frame {
                            scap::frame::VideoFrame::BGRA(bgra_frame) => {
                                let yuv = {
                                    let width = bgra_frame.width as u32;
                                    let height = bgra_frame.height as u32;

                                    // YUV420
                                    let y_size = width * height;
                                    let uv_size = (width / 2) * (height / 2);
                                    let total_size = y_size + uv_size * 2;

                                    let mut yuv_data = vec![0u8; total_size as usize];

                                    // YUV 버퍼를 Y, U, V 평면으로 분리
                                    let (y_plane, rest) = yuv_data.split_at_mut(y_size as usize);
                                    let (u_plane, v_plane) = rest.split_at_mut(uv_size as usize);

                                    let mut planar_image = YuvPlanarImageMut {
                                        y_plane: BufferStoreMut::Borrowed(y_plane),
                                        y_stride: width,
                                        u_plane: BufferStoreMut::Borrowed(u_plane),
                                        u_stride: width / 2,
                                        v_plane: BufferStoreMut::Borrowed(v_plane),
                                        v_stride: width / 2,
                                        width,
                                        height,
                                    };
                                    let bgra_stride = width * 4;

                                    bgra_to_yuv420(
                                        &mut planar_image,
                                        &bgra_frame.data,
                                        bgra_stride,
                                        YuvRange::Limited,
                                        YuvStandardMatrix::Bt709,
                                        YuvConversionMode::Fast,
                                    )?;

                                    YUVBuffer::from_vec(yuv_data, width as usize, height as usize)
                                };
                                FrameData {
                                    data: yuv,
                                    timestamp: bgra_frame.display_time,
                                }
                            }
                            _ => {
                                tracing::warn!(
                                    "Unsupported video frame format (expected YUV): {:?}",
                                    video_frame
                                );
                                continue;
                            }
                        },
                        Frame::Audio(_) => {
                            tracing::warn!("Received audio frame instead of video");
                            continue;
                        }
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
        // let mut current_width: u32 = 0;
        // let mut current_height: u32 = 0;

        loop {
            tokio::select! {
                Some(frame_data) = frame_rx.recv() => {
                    // Create encoder on first frame (encoder auto-reinitializes on resolution change)
                    if encoder.is_none() {
                        tracing::info!("Initializing H.264 encoder");
                        encoder = Some(Encoder::new()?);
                    }

                    if let Some(ref mut enc) = encoder {
                        let yuv = frame_data.data;

                        // Track dimension changes
                        // if frame_data.width != current_width || frame_data.height != current_height {
                        //     tracing::info!("Encoding {}x{} frames", frame_data.width, frame_data.height);
                        //     current_width = frame_data.width;
                        //     current_height = frame_data.height;
                        // }

                        // Encode frame with H.264 and collect all NAL units
                        let bitstream = match enc.encode(&yuv) {
                            Ok(encoded) => {
                                encoded.to_vec()
                            }
                            Err(e) => {
                                tracing::error!("H.264 encoding failed: {}", e);
                                Vec::new()
                            }
                        };

                        if let Err(e) = track.write_sample(&Sample {
                                data: Bytes::from(bitstream),
                                timestamp: frame_data.timestamp,
                                // duration: Duration::from_millis(FRAME_INTERVAL_MS),
                                ..Default::default()
                            })
                            .await {
                                tracing::error!("Failed to write RTP packet: {}", e);
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
