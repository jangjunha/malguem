## Next

* Voice Call and Screen Sharing
  * Want to check latency and quality first

## Future


  1. The screen capture currently sends placeholder data instead of actual encoded video frames
  2. Full VP8 encoding would need to be added for production use
  3. Track removal requires WebRTC renegotiation for complete cleanup
  4. Only captures the main display (could be extended to support multiple displays or window selection)


  - H.264 Baseline Profile encoding with openh264
  - MTU-aware packetization (1200 bytes) with FU-A fragmentation for large NAL units
  - 90kHz RTP clock for H.264 video
  - Payload type 96 for H.264
  - Proper handling of scap's Frame enum (Video/Audio variants) and VideoFrame formats (BGRA/BGR0)

화면 송출 시 스테레오믹스 같이

* Microphone sensitivity (threshold)
* Noice reduction

* Notification
* Online / Away / Offline
* Reactions to messages
* Custom stickers for channels

* Passwordless email auth
* Google Login linking

* Private channel

* Make available to configure self-hosted server

* iOS Client
