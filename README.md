## Requirements

### Features

#### User

* Username
* Profile Image (under 5MiB)

#### Channels

* Visibility
  * Public or Private
  * Anyone in server can see existence of channel but non channel member cannot see the contents
  * Only channel members can invite other member to the private channel
  * Anyone in server can join to the public channel
* Text Messages
  * Users can chat with text messages within channel
  * Text messages are stored in server
* RTC Sessions (= Discord voice call)
  * Any user within the channel can start the RTC session
  * There can be only one RTC session for a channel
  * A user can only participate in one RTC session at a time.
  * RTC session member can broadcast their screen too
* Broadcast
  * (Nice-to) User can select the broadcasting target - fullscreen or selected window
  * Streaming user can select the video quality (resolution and (nice-to) bitrate)
  * Do not support multiple options for video quality

### Technical Decisions

#### Components

* `Client`: User Client (Desktop)
* `Server`: Signaling Server

#### Authentication

* Use Firebase Authentication with Identity Platform for user authentication

#### Voice calls and Broadcasting

* P2P (Do NOT use SFU)
  * Should support STUN
* WebRTC-based
  * Do not use DTLS/SRTP.
  * Implement customized encrypted transport layer using AEGIS-256
    * Let's implement AEGIS-256 encryption last.
  * Use a UDP socket for each caller with multiple SSRC for audio and video(optional)
  * Use same (server-generated) encryption key within RTC session
    * The server generates a unique key for the session.
    * The sender encrypts data using the same key for all recipients.
      All recipients use the same key to decrypt data within the session.
* Encode video using H.264
  * Use GPU hardware accelration
* Use CapSoftware/scap for screen capturing for broadcast

#### Desktop Client

* Rust
* Use egui for GUI

#### Server

* Rust
* Use PostgreSQL for database
* Use tokio-postgres
* Use Tarpc for basic requests
* Use Websocket for realtime message processing (chat, signaling, ...)
* Assert single server. Do not consider scale out
* All user contents (without voice/video streaming itself) should E2E encrypted
  * Assert all users already shared a "server key" (AEGIS-256)
  * Server should not know "server key"
  * Should implemented at last
