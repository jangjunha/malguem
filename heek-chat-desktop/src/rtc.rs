use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::TrackLocal;
use interceptor::registry::Registry;

use heek_chat_lib::{ChannelID, ChatServiceClient, UserID};

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

        // Register default codecs (Opus for audio, VP8/VP9 for video)
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

        // Set up ICE candidate handler
        let ice_tx = ice_candidate_tx.clone();
        let peer_user_id = user_id.clone();
        peer_conn
            .on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
                let tx = ice_tx.clone();
                let uid = peer_user_id.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        let _ = tx.send((uid, candidate));
                    }
                })
            }));

        // Set up connection state handler
        peer_conn
            .on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
                tracing::info!("Peer connection state changed: {:?}", state);
                Box::pin(async {})
            }));

        // Set up track handler for receiving remote audio
        let runtime_for_track = Arc::clone(&runtime);
        peer_conn.on_track(Box::new(move |track, _, _| {
            tracing::info!("Received remote track: {} ({})", track.id(), track.kind());

            if track.kind() == webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio {
                // Start audio playback for this track
                let track_clone = Arc::clone(&track);
                let runtime_clone = Arc::clone(&runtime_for_track);
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
            }

            Box::pin(async {})
        }));

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
        use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
        use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
        use webrtc::rtp_transceiver::RTCRtpTransceiverInit;

        // Add a recvonly audio transceiver to ensure we have an m=audio line in the SDP
        let init = RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        };

        self.peer_conn.add_transceiver_from_kind(
            RTPCodecType::Audio,
            Some(init),
        ).await?;

        Ok(())
    }

    /// Create an offer for this peer connection
    pub async fn create_offer(&self) -> Result<RTCSessionDescription, Box<dyn std::error::Error>> {
        let offer = self.peer_conn.create_offer(None).await?;
        self.peer_conn.set_local_description(offer.clone()).await?;
        Ok(offer)
    }

    /// Set remote description (offer from remote peer)
    pub async fn set_remote_offer(
        &self,
        sdp: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let offer = RTCSessionDescription::offer(sdp)?;
        self.peer_conn.set_remote_description(offer).await?;

        // Process any pending ICE candidates
        self.process_pending_ice_candidates().await?;

        Ok(())
    }

    /// Create an answer for this peer connection
    pub async fn create_answer(&self) -> Result<RTCSessionDescription, Box<dyn std::error::Error>> {
        let answer = self.peer_conn.create_answer(None).await?;
        self.peer_conn.set_local_description(answer.clone()).await?;
        Ok(answer)
    }

    /// Set remote description (answer from remote peer)
    pub async fn set_remote_answer(
        &self,
        sdp: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
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

    /// Add an audio track to this peer connection
    pub async fn add_audio_track(
        &self,
        track: Arc<TrackLocalStaticRTP>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.peer_conn
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        Ok(())
    }

    /// Add a video track to this peer connection
    pub async fn add_video_track(
        &self,
        track: Arc<TrackLocalStaticRTP>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.peer_conn
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        Ok(())
    }

    /// Close this peer connection
    pub async fn close(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.peer_conn.close().await?;
        Ok(())
    }

    pub fn peer_connection(&self) -> Arc<RTCPeerConnection> {
        Arc::clone(&self.peer_conn)
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
    local_audio_track: Option<Arc<TrackLocalStaticRTP>>,
}

impl RTCSessionManager {
    /// Create a new RTC session manager
    pub fn new(
        channel_id: ChannelID,
        current_user_id: UserID,
        rpc_client: ChatServiceClient,
        firebase_token: String,
        runtime: Arc<tokio::runtime::Runtime>,
        local_audio_track: Option<Arc<TrackLocalStaticRTP>>,
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
            // Don't create a connection to ourselves
            return Ok(());
        }

        // Offer collision resolution: only the peer with the higher user ID creates the offer
        // This prevents both peers from sending offers simultaneously
        let should_create_offer = self.current_user_id > user_id;

        if !should_create_offer {
            tracing::info!("Not creating offer for user {} (waiting for their offer)", user_id);
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
        self.peers.lock().await.insert(user_id.clone(), peer.clone());

        // Add local audio track if available
        if let Some(track) = &self.local_audio_track {
            tracing::info!("Adding local audio track to peer connection");
            peer.add_audio_track(Arc::clone(track)).await?;
        } else {
            // Add audio transceiver to ensure m-line in SDP (recvonly if no local track)
            peer.add_audio_transceiver().await?;
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
            peer.add_audio_track(Arc::clone(track)).await?;
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
}
