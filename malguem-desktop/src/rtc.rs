use interceptor::registry::Registry;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Duration;
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use malguem_lib::{ChannelID, ChatServiceClient, UserID};

/// Represents a single peer connection to another user in an RTC session
pub struct PeerConnection {
    peer_conn: Arc<RTCPeerConnection>,
    user_id: UserID,
    channel_id: ChannelID,
    pending_ice_candidates: Arc<Mutex<Vec<String>>>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl PeerConnection {
    /// Create a new peer connection for a specific user
    pub async fn new(
        user_id: UserID,
        channel_id: ChannelID,
        ice_candidate_tx: mpsc::UnboundedSender<(UserID, RTCIceCandidate)>,
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Create a MediaEngine
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;

        // Create an InterceptorRegistry
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)?;

        // Create the API object with the MediaEngine
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // Configure ICE servers (Cloudflare STUN)
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.cloudflare.com:3478".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        // Create a new RTCPeerConnection
        let peer_conn = Arc::new(api.new_peer_connection(config).await?);

        // Allow us to receive 1 audio track, and 1 video track
        peer_conn
            .add_transceiver_from_kind(RTPCodecType::Audio, None)
            .await?;
        peer_conn
            .add_transceiver_from_kind(RTPCodecType::Video, None)
            .await?;

        // Set up ICE candidate handler
        let ice_tx = ice_candidate_tx.clone();
        let peer_user_id = user_id.clone();
        peer_conn.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let tx = ice_tx.clone();
            let uid = peer_user_id.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate {
                    let _ = tx.send((uid, candidate));
                }
            })
        }));

        // Set up connection state handler
        peer_conn.on_peer_connection_state_change(Box::new(
            move |state: RTCPeerConnectionState| {
                tracing::info!("Peer connection state changed: {:?}", state);
                Box::pin(async {})
            },
        ));

        // Set up track handler for receiving remote audio and video
        {
            let runtime_for_track = Arc::clone(&runtime);
            let peer_user_id_for_track = user_id.clone();
            let pc = Arc::downgrade(&peer_conn);
            peer_conn.on_track(Box::new(move |track, _, _| {
                let media_ssrc = track.ssrc();
                tracing::info!("===== RECEIVED REMOTE TRACK =====");
                tracing::info!("Track ID: {}", track.id());
                tracing::info!("Track Kind: {:?}", track.kind());
                tracing::info!("Track Codec: {:?}", track.codec());
                tracing::info!("Track SSRC: {:?}", media_ssrc);
                tracing::info!("User ID: {}", peer_user_id_for_track);
                tracing::info!("================================");

                // Send a PLI on an interval so that the publisher is pushing a keyframe every rtcpPLIInterval
                {
                    let pc = pc.clone();
                    runtime_for_track.spawn(async move {
                        let mut result = Result::<usize, webrtc::Error>::Ok(0);
                        while result.is_ok() {
                            let timeout = tokio::time::sleep(Duration::from_secs(3));
                            tokio::pin!(timeout);

                            tokio::select! {
                                _ = timeout.as_mut() =>{
                                    if let Some(pc) = pc.upgrade() {
                                        result = pc.write_rtcp(&[Box::new(PictureLossIndication{
                                            sender_ssrc: 0,
                                            media_ssrc,
                                        })]).await;
                                    } else {
                                        break;
                                    }
                                }
                            };
                        }
                    });
                };

                let track_clone = Arc::clone(&track);
                let runtime_clone = Arc::clone(&runtime_for_track);
                Box::pin(async move {
                    if track.kind() == RTPCodecType::Audio {
                        // Start audio playback for this track
                        std::thread::spawn(move || {
                            use crate::audio_playback::AudioPlayback;
                            match AudioPlayback::start(track_clone, runtime_clone) {
                                Ok(playback) => {
                                    tracing::info!("Audio playback started");
                                    // Keep playback alive by keeping it in scope
                                    std::thread::park();
                                    drop(playback);
                                }
                                Err(e) => {
                                    tracing::error!("Failed to start audio playback: {}", e);
                                }
                            }
                        });
                    } else if track.kind() == RTPCodecType::Video {
                        // Start video playback for this track
                        let user_id_for_video = peer_user_id_for_track.clone();
                        std::thread::spawn(move || {
                            use crate::video_playback::VideoPlayback;
                            match VideoPlayback::start(
                                user_id_for_video,
                                track_clone,
                                runtime_clone,
                            ) {
                                Ok(playback) => {
                                    tracing::info!("Video playback started");
                                    // Keep playback alive by keeping it in scope
                                    std::thread::park();
                                    drop(playback);
                                }
                                Err(e) => {
                                    tracing::error!("Failed to start video playback: {}", e);
                                }
                            }
                        });
                    }
                })
            }));
        };

        Ok(Self {
            peer_conn,
            user_id,
            channel_id,
            pending_ice_candidates: Arc::new(Mutex::new(Vec::new())),
            runtime,
        })
    }

    /// Add a transceiver for audio (creates an m-line in SDP)
    pub async fn add_audio_transceiver(&self) -> Result<(), Box<dyn std::error::Error>> {
        use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
        use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
        use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;

        // Add a recvonly audio transceiver to ensure we have an m=audio line in the SDP
        let init = RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        };

        self.peer_conn
            .add_transceiver_from_kind(RTPCodecType::Audio, Some(init))
            .await?;

        Ok(())
    }

    /// Add a transceiver for video (creates an m-line in SDP)
    pub async fn add_video_transceiver(&self) -> Result<(), Box<dyn std::error::Error>> {
        use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
        use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
        use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;

        // Add a recvonly video transceiver to ensure we have an m=video line in the SDP
        let init = RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        };

        self.peer_conn
            .add_transceiver_from_kind(RTPCodecType::Video, Some(init))
            .await?;

        Ok(())
    }

    /// Create an offer for this peer connection
    pub async fn create_offer(&self) -> Result<RTCSessionDescription, Box<dyn std::error::Error>> {
        let offer = self.peer_conn.create_offer(None).await?;
        tracing::debug!("Created offer SDP:\n{}", offer.sdp);
        self.peer_conn.set_local_description(offer.clone()).await?;
        Ok(offer)
    }

    /// Set remote description (offer from remote peer)
    pub async fn set_remote_offer(&self, sdp: String) -> Result<(), Box<dyn std::error::Error>> {
        tracing::debug!("Received offer SDP:\n{}", sdp);
        let offer = RTCSessionDescription::offer(sdp)?;
        self.peer_conn.set_remote_description(offer).await?;

        // Process any pending ICE candidates
        self.process_pending_ice_candidates().await?;

        Ok(())
    }

    /// Create an answer for this peer connection
    pub async fn create_answer(&self) -> Result<RTCSessionDescription, Box<dyn std::error::Error>> {
        let answer = self.peer_conn.create_answer(None).await?;
        tracing::debug!("Created answer SDP:\n{}", answer.sdp);
        self.peer_conn.set_local_description(answer.clone()).await?;
        Ok(answer)
    }

    /// Set remote description (answer from remote peer)
    pub async fn set_remote_answer(&self, sdp: String) -> Result<(), Box<dyn std::error::Error>> {
        tracing::debug!("Received answer SDP:\n{}", sdp);
        let answer = RTCSessionDescription::answer(sdp)?;
        self.peer_conn.set_remote_description(answer).await?;

        // Process any pending ICE candidates
        self.process_pending_ice_candidates().await?;

        Ok(())
    }

    /// Add an ICE candidate received from the remote peer
    pub async fn add_ice_candidate(
        &self,
        candidate: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Check if remote description is set
        if self.peer_conn.remote_description().await.is_none() {
            // Queue the candidate for later processing
            tracing::debug!("Queueing ICE candidate (remote description not set yet)");
            self.pending_ice_candidates.lock().await.push(candidate);
            return Ok(());
        }

        let init = serde_json::from_str::<RTCIceCandidateInit>(&candidate)?;
        self.peer_conn.add_ice_candidate(init).await?;
        Ok(())
    }

    /// Process all pending ICE candidates
    async fn process_pending_ice_candidates(&self) -> Result<(), Box<dyn std::error::Error>> {
        let candidates = {
            let mut pending = self.pending_ice_candidates.lock().await;
            std::mem::take(&mut *pending)
        };

        if !candidates.is_empty() {
            tracing::info!("Processing {} pending ICE candidates", candidates.len());
            for candidate in candidates {
                let init = serde_json::from_str::<RTCIceCandidateInit>(&candidate)?;
                self.peer_conn.add_ice_candidate(init).await?;
            }
        }

        Ok(())
    }

    /// Add a track to this peer connection
    pub async fn add_track(
        &self,
        track: Arc<TrackLocalStaticSample>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sender = self
            .peer_conn
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        // Read incoming RTCP packets
        // Before these packets are returned they are processed by interceptors. For things
        // like NACK this needs to be called.
        self.runtime.spawn(async move {
            let mut rtcp_buf = vec![0u8; 1500];
            while let Ok((_, _)) = sender.read(&mut rtcp_buf).await {}
            Result::<(), ()>::Ok(())
        });

        Ok(())
    }

    /// Close this peer connection
    pub async fn close(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.peer_conn.close().await?;
        Ok(())
    }
}

/// Manages all peer connections for an RTC session
pub struct RTCSessionManager {
    channel_id: ChannelID,
    current_user_id: UserID,
    peers: Arc<Mutex<HashMap<UserID, Arc<PeerConnection>>>>,
    rpc_client: ChatServiceClient,
    firebase_token: String,
    ice_candidate_tx: mpsc::UnboundedSender<(UserID, RTCIceCandidate)>,
    ice_candidate_rx: Arc<Mutex<mpsc::UnboundedReceiver<(UserID, RTCIceCandidate)>>>,
    runtime: Arc<tokio::runtime::Runtime>,
    local_audio_track: Option<Arc<TrackLocalStaticSample>>,
    local_screen_track: Arc<Mutex<Option<Arc<TrackLocalStaticSample>>>>,
}

impl RTCSessionManager {
    /// Create a new RTC session manager
    pub fn new(
        channel_id: ChannelID,
        current_user_id: UserID,
        rpc_client: ChatServiceClient,
        firebase_token: String,
        runtime: Arc<tokio::runtime::Runtime>,
        local_audio_track: Option<Arc<TrackLocalStaticSample>>,
    ) -> Self {
        let (ice_candidate_tx, ice_candidate_rx) = mpsc::unbounded_channel();

        Self {
            channel_id,
            current_user_id,
            peers: Arc::new(Mutex::new(HashMap::new())),
            rpc_client,
            firebase_token,
            ice_candidate_tx,
            ice_candidate_rx: Arc::new(Mutex::new(ice_candidate_rx)),
            runtime,
            local_audio_track,
            local_screen_track: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the ICE candidate sender task
    pub fn start_ice_candidate_sender(&self) {
        let rx = Arc::clone(&self.ice_candidate_rx);
        let rpc_client = self.rpc_client.clone();
        let firebase_token = self.firebase_token.clone();
        let channel_id = self.channel_id.clone();

        self.runtime.spawn(async move {
            let mut rx = rx.lock().await;
            while let Some((target_user_id, candidate)) = rx.recv().await {
                // Serialize the ICE candidate to JSON
                let candidate_json = match candidate.to_json() {
                    Ok(init) => match serde_json::to_string(&init) {
                        Ok(json) => json,
                        Err(e) => {
                            tracing::error!("Failed to serialize ICE candidate: {}", e);
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::error!("Failed to convert ICE candidate to JSON: {}", e);
                        continue;
                    }
                };

                // Send ICE candidate to server
                let result = rpc_client
                    .ice_candidate_rtc_connection(
                        tarpc::context::current(),
                        firebase_token.clone(),
                        channel_id.clone(),
                        target_user_id,
                        candidate_json,
                    )
                    .await;

                if let Err(e) = result {
                    tracing::error!("Failed to send ICE candidate: {}", e);
                }
            }
        });
    }

    /// Handle when a new user joins the RTC session
    pub async fn handle_user_joined(
        &self,
        user_id: UserID,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if user_id == self.current_user_id {
            return Ok(());
        }

        // Offer collision resolution: only the peer with the higher user ID creates the offer
        // This prevents both peers from sending offers simultaneously
        let should_create_offer = self.current_user_id > user_id;
        if !should_create_offer {
            tracing::info!(
                "Not creating offer for user {} (waiting for their offer)",
                user_id
            );
            return Ok(());
        }

        tracing::info!("Creating peer connection for user {}", user_id);

        // Create a new peer connection
        let peer = Arc::new(
            PeerConnection::new(
                user_id.clone(),
                self.channel_id.clone(),
                self.ice_candidate_tx.clone(),
                Arc::clone(&self.runtime),
            )
            .await?,
        );

        // Store the peer connection
        self.peers
            .lock()
            .await
            .insert(user_id.clone(), peer.clone());

        // Add local audio track if available
        if let Some(track) = &self.local_audio_track {
            tracing::info!("Adding local audio track to peer connection");
            peer.add_track(Arc::clone(track)).await?;
        }

        // Add local screen track if available
        if let Some(track) = &*self.local_screen_track.lock().await {
            tracing::info!("Adding local screen track to peer connection");
            peer.add_track(Arc::clone(track)).await?;
        }

        // Create and send offer
        let offer = peer.create_offer().await?;

        tracing::info!("Sending offer to user {}", user_id);
        self.rpc_client
            .offer_rtc_connection(
                tarpc::context::current(),
                self.firebase_token.clone(),
                self.channel_id.clone(),
                user_id,
                offer.sdp,
            )
            .await??;

        Ok(())
    }

    /// Handle when a user leaves the RTC session
    pub async fn handle_user_left(
        &self,
        user_id: UserID,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("Removing peer connection for user {}", user_id);

        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.remove(&user_id) {
            peer.close().await?;
        }

        Ok(())
    }

    /// Handle receiving an offer from a remote peer
    pub async fn handle_offer_received(
        &self,
        from_user_id: UserID,
        sdp: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if from_user_id == self.current_user_id {
            return Ok(());
        }

        tracing::info!("Received offer from user {}", from_user_id);

        // Create a new peer connection if it doesn't exist
        let peer = {
            let mut peers = self.peers.lock().await;
            if let Some(peer) = peers.get(&from_user_id) {
                Arc::clone(peer)
            } else {
                let new_peer = Arc::new(
                    PeerConnection::new(
                        from_user_id.clone(),
                        self.channel_id.clone(),
                        self.ice_candidate_tx.clone(),
                        Arc::clone(&self.runtime),
                    )
                    .await?,
                );
                peers.insert(from_user_id.clone(), new_peer.clone());
                new_peer
            }
        };

        // Add local audio track if available (before setting remote description)
        if let Some(track) = &self.local_audio_track {
            tracing::info!("Adding local audio track to peer connection");
            peer.add_track(Arc::clone(track)).await?;
        }

        // Add local screen track if available (before setting remote description)
        if let Some(track) = &*self.local_screen_track.lock().await {
            tracing::info!("Adding local screen track to peer connection");
            peer.add_track(Arc::clone(track)).await?;
        }

        // Set the remote offer
        peer.set_remote_offer(sdp).await?;

        // Create and send answer
        let answer = peer.create_answer().await?;

        tracing::info!("Sending answer to user {}", from_user_id);
        self.rpc_client
            .answer_rtc_connection(
                tarpc::context::current(),
                self.firebase_token.clone(),
                self.channel_id.clone(),
                from_user_id,
                answer.sdp,
            )
            .await??;

        Ok(())
    }

    /// Handle receiving an answer from a remote peer
    pub async fn handle_answer_received(
        &self,
        from_user_id: UserID,
        sdp: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if from_user_id == self.current_user_id {
            return Ok(());
        }

        tracing::info!("Received answer from user {}", from_user_id);

        let peers = self.peers.lock().await;
        if let Some(peer) = peers.get(&from_user_id) {
            peer.set_remote_answer(sdp).await?;
        } else {
            tracing::warn!("Received answer from unknown peer {}", from_user_id);
        }

        Ok(())
    }

    /// Handle receiving an ICE candidate from a remote peer
    pub async fn handle_ice_candidate_received(
        &self,
        from_user_id: UserID,
        candidate: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if from_user_id == self.current_user_id {
            return Ok(());
        }

        tracing::debug!("Received ICE candidate from user {}", from_user_id);

        let peers = self.peers.lock().await;
        if let Some(peer) = peers.get(&from_user_id) {
            peer.add_ice_candidate(candidate).await?;
        } else {
            tracing::warn!("Received ICE candidate from unknown peer {}", from_user_id);
        }

        Ok(())
    }

    /// Close all peer connections
    pub async fn close_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut peers = self.peers.lock().await;
        for (user_id, peer) in peers.drain() {
            tracing::info!("Closing peer connection for user {}", user_id);
            peer.close().await?;
        }
        Ok(())
    }

    /// Get a peer connection by user ID
    pub async fn get_peer(&self, user_id: &UserID) -> Option<Arc<PeerConnection>> {
        self.peers.lock().await.get(user_id).map(Arc::clone)
    }

    /// Set screen sharing track and add it to all existing peer connections
    pub async fn set_screen_track(
        &self,
        track: Arc<TrackLocalStaticSample>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("===== SETTING SCREEN TRACK =====");

        // Store the track
        *self.local_screen_track.lock().await = Some(Arc::clone(&track));

        // Add to all existing peer connections and renegotiate
        let peers = self.peers.lock().await;
        tracing::info!("Number of existing peers: {}", peers.len());

        for (user_id, peer) in peers.iter() {
            tracing::info!(
                "Adding screen track to peer connection for user {}",
                user_id
            );
            peer.add_track(Arc::clone(&track)).await?;

            // Renegotiate by creating and sending a new offer
            tracing::info!(
                "Renegotiating connection with user {} after adding video track",
                user_id
            );
            let offer = peer.create_offer().await?;

            tracing::info!("Sending renegotiation offer to user {}", user_id);
            self.rpc_client
                .offer_rtc_connection(
                    tarpc::context::current(),
                    self.firebase_token.clone(),
                    self.channel_id.clone(),
                    user_id.clone(),
                    offer.sdp,
                )
                .await??;
            tracing::info!("Renegotiation offer sent successfully to user {}", user_id);
        }

        tracing::info!("===== SCREEN TRACK SET COMPLETE =====");
        Ok(())
    }

    /// Remove screen sharing track from all peer connections
    pub async fn remove_screen_track(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Clear the stored track
        *self.local_screen_track.lock().await = None;

        // Note: WebRTC doesn't have a direct way to remove tracks after they've been added
        // In a production system, you would need to renegotiate the connection
        // For now, we just clear the reference
        tracing::info!(
            "Screen sharing track removed (renegotiation required for complete removal)"
        );

        Ok(())
    }
}
