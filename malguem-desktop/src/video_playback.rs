use eframe::egui;
use malguem_lib::UserID;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
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
        let mut decoder = Decoder::new()?;

        // Create SampleBuilder with H264Packet depacketizer
        // max_late: 512, sample_rate: 90000 (H.264 clock rate)
        let mut sample_builder = SampleBuilder::new(512, H264Packet::default(), 90000);

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
                let bitstream: &[u8] = &sample.data;
                if let Err(e) =
                    Self::decode_and_display(&mut decoder, bitstream, &frame_buffer).await
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
        decoder: &mut Decoder,
        bitstream: &[u8],
        frame_buffer: &Arc<Mutex<Option<VideoFrame>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Decode the NAL unit
        let yuv = decoder.decode(bitstream)?;

        if let Some(yuv_frame) = yuv {
            // Get dimensions
            let (width, height) = yuv_frame.dimensions();
            let width = width as u32;
            let height = height as u32;

            // Convert YUV420 to RGBA for display
            let rgba_data = Self::yuv420_to_rgba(&yuv_frame, width, height)?;

            // Update frame buffer
            let frame = VideoFrame {
                width,
                height,
                rgba_data,
            };

            let mut buffer = frame_buffer.lock().await;
            *buffer = Some(frame);
        }

        Ok(())
    }

    /// Convert YUV420 to RGBA
    fn yuv420_to_rgba(
        yuv_frame: &impl openh264::formats::YUVSource,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let width_usize = width as usize;
        let height_usize = height as usize;

        let (y_stride, u_stride, v_stride) = yuv_frame.strides();
        let yuv_image = YuvPlanarImage {
            y_plane: yuv_frame.y(),
            y_stride: y_stride as u32,
            u_plane: yuv_frame.u(),
            u_stride: u_stride as u32,
            v_plane: yuv_frame.v(),
            v_stride: v_stride as u32,
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
