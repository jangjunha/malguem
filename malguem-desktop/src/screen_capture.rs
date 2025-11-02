use ac_ffmpeg::codec::Encoder;
use ac_ffmpeg::codec::video::{self, VideoEncoder, VideoFrameMut};
use ac_ffmpeg::time::TimeBase;
use bytes::Bytes;
use scap::capturer::{Capturer, Options};
use scap::frame::Frame;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use webrtc::api::media_engine::MIME_TYPE_H264;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use yuv::{
    BufferStoreMut, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    bgra_to_yuv420,
};

use crate::codec::find_encoder;

const TARGET_FPS: u32 = 30;
const FRAME_INTERVAL_MS: u64 = 1000 / TARGET_FPS as u64;
const H264_CLOCK_RATE: u32 = 90000;

pub struct FrameData {
    pub yuv_data: Vec<u8>,
    pub width: u32,
    pub height: u32,
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
        // Use bounded channel with capacity 1 to minimize latency
        let (frame_tx, mut frame_rx) = mpsc::channel::<FrameData>(1);
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
        frame_tx: mpsc::Sender<FrameData>,
        stop_tx: mpsc::UnboundedSender<()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("capture_task()");

        // Create capturer options
        let options = Options {
            fps: 30,
            target: None, // None captures the primary display
            output_type: scap::frame::FrameType::BGRAFrame,
            output_resolution: scap::capturer::Resolution::_1080p,
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
                                let capture_time = bgra_frame.display_time;
                                tracing::info!(
                                    "[LATENCY] capture delay: {}ms",
                                    capture_time.elapsed().unwrap().as_millis()
                                );

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

                                let yuv_conversion_ms = capture_time.elapsed().unwrap().as_millis();
                                tracing::info!(
                                    "[LATENCY] until YUV conversion: {}ms",
                                    yuv_conversion_ms
                                );

                                FrameData {
                                    yuv_data,
                                    width,
                                    height,
                                    timestamp: capture_time,
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

                    // Use try_send to drop frame if channel is full (keeps latency low)
                    match frame_tx.try_send(frame_data) {
                        Ok(_) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            // Channel full - skip this frame to avoid latency buildup
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            tracing::warn!("Failed to send frame, receiver dropped");
                            break;
                        }
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
        frame_rx: &mut mpsc::Receiver<FrameData>,
        stop_rx: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // H.264 encoder (created on first frame)
        let mut encoder: Option<VideoEncoder> = None;
        let mut frame_counter: i64 = 0;

        loop {
            tokio::select! {
                Some(frame_data) = frame_rx.recv() => {
                    // Create encoder on first frame
                    if encoder.is_none() {
                        tracing::info!("Initializing H.264 encoder with ac-ffmpeg ({}x{})", frame_data.width, frame_data.height);
                        let enc = find_encoder(frame_data.width as usize, frame_data.height as usize, TARGET_FPS)?;
                        encoder = Some(enc);
                    }

                    if let Some(ref mut enc) = encoder {
                        let t0 = std::time::SystemTime::now();

                        // Create a mutable video frame
                        let pixel_format = video::frame::get_pixel_format("yuv420p");
                        let time_base = TimeBase::new(1, TARGET_FPS as i32);
                        let mut video_frame = VideoFrameMut::black(
                            pixel_format,
                            frame_data.width as usize,
                            frame_data.height as usize
                        );

                        // Copy YUV data into the frame
                        // YUV420 planar format: Y plane, then U plane, then V plane
                        let y_size = (frame_data.width * frame_data.height) as usize;
                        let uv_size = y_size / 4;

                        {
                            let mut frame_planes = video_frame.planes_mut();
                            frame_planes[0].data_mut()[..y_size].copy_from_slice(&frame_data.yuv_data[..y_size]);
                            frame_planes[1].data_mut()[..uv_size].copy_from_slice(&frame_data.yuv_data[y_size..y_size + uv_size]);
                            frame_planes[2].data_mut()[..uv_size].copy_from_slice(&frame_data.yuv_data[y_size + uv_size..]);
                        }

                        // Set presentation timestamp and freeze to immutable frame
                        // let pts = Timestamp::new(frame_counter, time_base);
                        // let video_frame = video_frame.freeze().with_pts(pts);
                        // frame_counter += 1;
                        let video_frame = video_frame.freeze();

                        // Encode the frame
                        enc.push(video_frame)?;

                        // Collect encoded packets
                        let mut bitstream = Vec::new();
                        while let Some(packet) = enc.take()? {
                            bitstream.extend_from_slice(packet.data());
                        }

                        let encoding_ms = t0.elapsed().unwrap().as_millis();
                        let bitstream_size = bitstream.len();
                        tracing::info!("[LATENCY] H.264 encoding: {}ms, size: {} bytes", encoding_ms, bitstream_size);

                        if !bitstream.is_empty() {
                            let t1 = std::time::SystemTime::now();
                            let send_timestamp = SystemTime::now();
                            if let Err(e) = track.write_sample(&Sample {
                                    data: Bytes::from(bitstream),
                                    timestamp: frame_data.timestamp,
                                    duration: Duration::from_millis(FRAME_INTERVAL_MS),
                                    ..Default::default()
                                })
                                .await {
                                    tracing::error!("Failed to write RTP packet: {}", e);
                            }

                            let write_ms = t1.elapsed().unwrap().as_millis();
                            tracing::info!("[LATENCY] RTP write: {}ms, total: {}ms, sent at: {:?}", write_ms, frame_data.timestamp.elapsed().unwrap().as_millis(), send_timestamp);
                        }
                    }
                }
                Some(_) = stop_rx.recv() => {
                    tracing::info!("Stop signal received, ending frame processing");

                    // Flush the encoder
                    if let Some(ref mut enc) = encoder {
                        let _ = enc.flush();
                    }

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
