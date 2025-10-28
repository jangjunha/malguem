use eframe::egui;
use malguem_lib::UserID;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use std::sync::Arc;
use tokio::sync::Mutex;
use webrtc::track::track_remote::TrackRemote;
use yuv::{YuvPlanarImage, YuvRange, YuvStandardMatrix};

/// Represents a video playback session for a remote video track
pub struct VideoPlayback {
    #[allow(dead_code)]
    user_id: UserID,
    #[allow(dead_code)]
    frame_buffer: Arc<Mutex<Option<VideoFrame>>>,
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

        // Create a separate window for this video stream
        let frame_buffer_for_window = Arc::clone(&frame_buffer);
        let user_id_for_window = user_id;
        std::thread::spawn(move || {
            Self::video_window_task(user_id_for_window, frame_buffer_for_window);
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

        tracing::info!("Starting video receive loop for track: {}", track.id());

        // Buffer to accumulate RTP payload data for a complete frame
        let mut nal_buffer = Vec::new();

        loop {
            // Read RTP packet
            let (rtp_packet, _) = match track.read_rtp().await {
                Ok(result) => result,
                Err(e) => {
                    tracing::debug!("Video RTP read error: {}", e);
                    break;
                }
            };

            // H.264 RTP payload processing
            // The payload contains NAL units (may be fragmented via FU-A)
            let payload = &rtp_packet.payload;

            if payload.is_empty() {
                continue;
            }

            // Check NAL unit type (first 5 bits of first byte)
            let nal_type = payload[0] & 0x1F;

            if nal_type == 28 {
                // FU-A fragmentation unit
                if payload.len() < 2 {
                    continue;
                }

                let fu_header = payload[1];
                let start_bit = (fu_header & 0x80) != 0;
                let end_bit = (fu_header & 0x40) != 0;
                let fragment_type = fu_header & 0x1F;

                if start_bit {
                    // Start of fragmented NAL unit
                    nal_buffer.clear();
                    // Reconstruct NAL header
                    let nal_header = (payload[0] & 0xE0) | fragment_type;
                    nal_buffer.push(nal_header);
                    nal_buffer.extend_from_slice(&payload[2..]);
                } else {
                    // Continuation or end of fragmented NAL unit
                    nal_buffer.extend_from_slice(&payload[2..]);
                }

                if end_bit {
                    // Complete NAL unit assembled, decode it
                    if let Err(e) =
                        Self::decode_and_display(&mut decoder, &nal_buffer, &frame_buffer).await
                    {
                        tracing::debug!("Failed to decode NAL unit: {}", e);
                    }
                    nal_buffer.clear();
                }
            } else {
                // Single NAL unit (not fragmented)
                if let Err(e) = Self::decode_and_display(&mut decoder, payload, &frame_buffer).await
                {
                    tracing::debug!("Failed to decode NAL unit: {}", e);
                }
            }
        }

        tracing::info!("Video receive loop ended for track: {}", track.id());
        Ok(())
    }

    /// Decode a NAL unit and update the frame buffer
    async fn decode_and_display(
        decoder: &mut Decoder,
        nal_data: &[u8],
        frame_buffer: &Arc<Mutex<Option<VideoFrame>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Decode the NAL unit
        let yuv = decoder.decode(nal_data)?;

        if let Some(yuv_frame) = yuv {
            // Get dimensions
            let (width, height) = yuv_frame.dimensions();
            let width = width as u32;
            let height = height as u32;

            // Convert YUV420 to BGRA for display
            let bgra_data = Self::yuv420_to_bgra(&yuv_frame, width, height)?;

            // Update frame buffer
            let frame = VideoFrame {
                width,
                height,
                rgba_data: bgra_data,
            };

            let mut buffer = frame_buffer.lock().await;
            *buffer = Some(frame);
        }

        Ok(())
    }

    /// Convert YUV420 to BGRA
    fn yuv420_to_bgra(
        yuv_frame: &impl openh264::formats::YUVSource,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let width_usize = width as usize;
        let height_usize = height as usize;

        // Create YUV planar image from openh264 output
        let (y_stride, u_stride, v_stride) = yuv_frame.strides();
        let yuv_image = YuvPlanarImage {
            y_plane: yuv_frame.y(),
            y_stride: y_stride as u32,
            u_plane: yuv_frame.u(),
            u_stride: u_stride as u32,
            v_plane: yuv_frame.v(),
            v_stride: v_stride as u32,
            width: width,
            height: height,
        };

        // Allocate BGRA buffer
        let bgra_size = width_usize * height_usize * 4;
        let mut bgra_data = vec![0u8; bgra_size];

        // Convert YUV420 to BGRA
        yuv::yuv420_to_bgra(
            &yuv_image,
            &mut bgra_data,
            (width_usize * 4) as u32, // BGRA stride is width * 4 bytes per pixel
            YuvRange::Limited,
            YuvStandardMatrix::Bt601,
        )?;

        Ok(bgra_data)
    }

    /// Create and run a video display window
    fn video_window_task(user_id: UserID, frame_buffer: Arc<Mutex<Option<VideoFrame>>>) {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(format!("Video - User {}", user_id))
                .with_inner_size([640.0, 480.0]),
            ..Default::default()
        };

        let _ = eframe::run_simple_native(
            &format!("Video - User {}", user_id),
            options,
            move |ctx, _frame| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // Try to get the latest frame
                    if let Ok(buffer) = frame_buffer.try_lock() {
                        if let Some(ref video_frame) = *buffer {
                            // Create a color image from BGRA data
                            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                [video_frame.width as usize, video_frame.height as usize],
                                &video_frame.rgba_data,
                            );

                            let texture = ctx.load_texture(
                                "video_frame",
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

                // Request continuous repaints for smooth video playback
                ctx.request_repaint();
            },
        );

        tracing::info!("Video window closed for user: {}", user_id);
    }
}
