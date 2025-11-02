use ac_ffmpeg::codec::video::{self, VideoEncoder};
use ac_ffmpeg::time::TimeBase;

pub fn find_encoder(
    width: usize,
    height: usize,
    fps: u32,
) -> Result<VideoEncoder, Box<dyn std::error::Error>> {
    let pixel_format = video::frame::get_pixel_format("yuv420p");
    let time_base = TimeBase::new(1, fps as i32);

    let builder = if let Ok(builder) = VideoEncoder::builder("h264_nvenc") {
        tracing::info!("Use h264_nvenc");
        // NVIDIA GPU
        builder
            .set_option("preset", "p1")
            .set_option("tune", "ll")
            .set_option("bf", "0")
            .set_option("profile:v", "main")
            .set_option("g", "30")
            .set_option("keyint_min", "30")
    } else if let Ok(builder) = VideoEncoder::builder("h264_qsv") {
        tracing::info!("Use h264_qsv");
        // Intel Quick Sync
        builder
            .set_option("preset", "veryfast")
            .set_option("profile:v", "main")
            .set_option("bf", "0")
            .set_option("g", "30")
            .set_option("look_ahead", "0")
            .set_option("async_depth", "1")
    } else if let Ok(builder) = VideoEncoder::builder("h264_videotoolbox") {
        tracing::info!("Use h264_videotoolbox");
        // macOS/iOS
        builder
            .set_option("profile:v", "main")
            .set_option("gop_size", "30")
            .set_option("allow_sw", "1")
            .set_option("realtime", "1")
    } else if let Ok(builder) = VideoEncoder::builder("h264_vaapi") {
        tracing::info!("Use h264_vaapi");
        // Linux VA-API
        builder
            .set_option("profile:v", "main")
            .set_option("bf", "0")
            .set_option("g", "30")
    } else if let Ok(builder) = VideoEncoder::builder("h264_amf") {
        tracing::info!("Use h264_amf");
        // AMD GPU
        builder
            .set_option("quality", "speed")
            .set_option("profile:v", "main")
            .set_option("bf", "0")
            .set_option("g", "30")
    } else if let Ok(builder) = VideoEncoder::builder("libx264") {
        tracing::info!("Use libxh264");
        builder
            .set_option("preset", "ultrafast") // Fastest encoding
            .set_option("tune", "zerolatency") // Zero latency tuning
            .set_option("bframes", "0") // Disable B-frames for lower latency
            .set_option("profile:v", "main")
            .set_option("g", "30")
            .set_option("keyint_min", "30")
            .set_option("sc_threshold", "0")
    } else {
        return Err("Cannot find encoder".into());
    };

    Ok(builder
        .width(width)
        .height(height)
        .pixel_format(pixel_format)
        .time_base(time_base)
        .bit_rate(3_000_000)
        .build()?)
}
