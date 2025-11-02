use ac_ffmpeg::codec::Decoder;
use ac_ffmpeg::codec::video::VideoDecoder;
use ac_ffmpeg::packet::PacketMut;
use eframe::egui;
use malguem_lib::UserID;
use std::sync::Arc;
use tokio::sync::Mutex;
use webrtc::media::io::sample_builder::SampleBuilder;
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::track::track_remote::TrackRemote;
use yuv::{YuvPlanarImage, YuvRange, YuvStandardMatrix};

/// Represents a video playback session for a remote video track
pub struct VideoPlayback {
    pub user_id: UserID,
    pub frame_buffer: Arc<Mutex<Option<VideoFrame>>>,
}

/// A decoded video frame ready for display
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
}

impl VideoPlayback {
    /// Start playing video from a remote track
    pub fn start(
        user_id: UserID,
        track: Arc<TrackRemote>,
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        tracing::info!("Starting video playback for user: {}", user_id);

        // Create shared buffer for decoded frames
        let frame_buffer = Arc::new(Mutex::new(None));
        let frame_buffer_clone = Arc::clone(&frame_buffer);

        // Spawn task to receive and decode video packets
        runtime.spawn(async move {
            if let Err(e) = Self::video_receive_task(track, frame_buffer_clone).await {
                tracing::error!("Video receive task failed: {}", e);
            }
        });

        Ok(Self {
            user_id,
            frame_buffer,
        })
    }

    /// Task to receive and decode H.264 RTP packets
    async fn video_receive_task(
        track: Arc<TrackRemote>,
        frame_buffer: Arc<Mutex<Option<VideoFrame>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create H.264 decoder
        let mut decoder = VideoDecoder::new("h264")?;

        // Create SampleBuilder with H264Packet depacketizer
        // max_late: 80 (balanced for 1080p I-frames without excessive buffering), sample_rate: 90000 (H.264 clock rate)
        let mut sample_builder = SampleBuilder::new(64, H264Packet::default(), 90000);

        tracing::info!("Starting video receive loop for track: {}", track.id());

        loop {
            // Read RTP packet
            let (rtp_packet, _) = match track.read_rtp().await {
                Ok(result) => result,
                Err(e) => {
                    tracing::debug!("Video RTP read error: {}", e);
                    break;
                }
            };

            // Push RTP packet into SampleBuilder
            sample_builder.push(rtp_packet);

            // Pop completed samples
            while let Some(sample) = sample_builder.pop() {
                let receive_time = std::time::SystemTime::now();
                let sample_size = sample.data.len();

                tracing::info!(
                    "[LATENCY] Sample popped from builder, size: {} bytes, received at: {:?}",
                    sample_size,
                    receive_time
                );

                if let Err(e) = Self::decode_and_display(
                    &mut decoder,
                    &sample.data,
                    &frame_buffer,
                    receive_time,
                )
                .await
                {
                    tracing::debug!("Failed to decode: {}", e);
                }
            }
        }

        tracing::info!("Video receive loop ended for track: {}", track.id());
        Ok(())
    }

    /// Decode a NAL unit and update the frame buffer
    async fn decode_and_display(
        decoder: &mut VideoDecoder,
        bitstream: &[u8],
        frame_buffer: &Arc<Mutex<Option<VideoFrame>>>,
        t0: std::time::SystemTime,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create packet from bitstream
        let mut packet_mut = PacketMut::new(bitstream.len());
        packet_mut.data_mut().copy_from_slice(bitstream);
        let packet = packet_mut.freeze();

        // Push packet to decoder
        decoder.push(packet)?;

        // Try to get decoded frames
        while let Some(frame) = decoder.take()? {
            let decoding_ms = t0.elapsed().unwrap().as_millis();
            tracing::info!("[LATENCY] H.264 decoding: {}ms", decoding_ms);

            let t1 = std::time::SystemTime::now();

            // Get dimensions
            let width = frame.width() as u32;
            let height = frame.height() as u32;

            // Convert YUV420 to RGBA for display
            let rgba_data = Self::yuv420_to_rgba_ffmpeg(&frame, width, height)?;

            let rgba_conversion_ms = t1.elapsed().unwrap().as_millis();
            tracing::info!("[LATENCY] RGBA conversion: {}ms", rgba_conversion_ms);

            // Update frame buffer
            let video_frame = VideoFrame {
                width,
                height,
                rgba_data,
            };

            let t2 = std::time::SystemTime::now();
            let mut buffer = frame_buffer.lock().await;
            *buffer = Some(video_frame);
            let buffer_update_ms = t2.elapsed().unwrap().as_millis();

            let total_ms = t0.elapsed().unwrap().as_millis();
            tracing::info!(
                "[LATENCY] Frame buffer update: {}ms, TOTAL receive pipeline: {}ms",
                buffer_update_ms,
                total_ms
            );
        }

        Ok(())
    }

    /// Convert YUV420 to RGBA using ac-ffmpeg frame
    fn yuv420_to_rgba_ffmpeg(
        yuv_frame: &ac_ffmpeg::codec::video::VideoFrame,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let width_usize = width as usize;
        let height_usize = height as usize;

        let planes = yuv_frame.planes();

        // Get Y, U, V plane data and strides
        let y_plane = planes[0].data();
        let u_plane = planes[1].data();
        let v_plane = planes[2].data();

        let y_stride = planes[0].line_size() as u32;
        let u_stride = planes[1].line_size() as u32;
        let v_stride = planes[2].line_size() as u32;

        let yuv_image = YuvPlanarImage {
            y_plane,
            y_stride,
            u_plane,
            u_stride,
            v_plane,
            v_stride,
            width,
            height,
        };

        // Allocate RGBA buffer
        let rgba_size = width_usize * height_usize * 4;
        let mut rgba_data = vec![0u8; rgba_size];

        // Convert YUV420 to RGBA
        yuv::yuv420_to_rgba(
            &yuv_image,
            &mut rgba_data,
            (width_usize * 4) as u32, // RGBA stride is width * 4 bytes per pixel
            YuvRange::Limited,
            YuvStandardMatrix::Bt709,
        )?;

        Ok(rgba_data)
    }

    /// Render video in an egui::Window
    /// Call this from the main UI loop
    pub fn render_window(&self, ctx: &egui::Context) {
        egui::Window::new(format!("Video - User {}", self.user_id))
            .default_size([640.0, 480.0])
            .show(ctx, |ui| {
                // Try to get the latest frame
                if let Ok(buffer) = self.frame_buffer.try_lock() {
                    if let Some(ref video_frame) = *buffer {
                        // Create a color image from RGBA data
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [video_frame.width as usize, video_frame.height as usize],
                            &video_frame.rgba_data,
                        );

                        let texture = ctx.load_texture(
                            format!("video_frame_{}", self.user_id),
                            color_image,
                            egui::TextureOptions::default(),
                        );

                        // Display the video frame
                        ui.image(&texture);
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Waiting for video...");
                        });
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("Loading...");
                    });
                }
            });
    }
}
