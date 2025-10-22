use serde::{Deserialize, Serialize};

const FIREBASE_API_KEY: &str = "AIzaSyDddynxPugKEM6aCTKjOxsmiPvZurBCfVo";
const FIREBASE_AUTH_CLIENT_ID: &str =
    "447805294208-26bu71mrsijvcucn8vrkn2tu2oo63o4i.apps.googleusercontent.com";
const FIREBASE_AUTH_API: &str = "https://identitytoolkit.googleapis.com/v1";
const FIREBASE_TOKEN_API: &str = "https://securetoken.googleapis.com/v1";

#[derive(Debug, Clone)]
pub struct FirebaseAuth {
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    #[serde(rename = "idToken")]
    pub id_token: String,
    #[serde(rename = "refreshToken")]
    pub refresh_token: String,
    #[serde(rename = "localId")]
    pub local_id: String,
    pub email: Option<String>,
    #[serde(rename = "expiresIn")]
    pub expires_in: String,
}

#[derive(Debug, Serialize)]
struct SignInWithPasswordRequest {
    email: String,
    password: String,
    #[serde(rename = "returnSecureToken")]
    return_secure_token: bool,
}

#[derive(Debug, Serialize)]
struct SignUpRequest {
    email: String,
    password: String,
    #[serde(rename = "returnSecureToken")]
    return_secure_token: bool,
}

#[derive(Debug, Serialize)]
struct SignInWithIdpRequest {
    #[serde(rename = "postBody")]
    post_body: String,
    #[serde(rename = "requestUri")]
    request_uri: String,
    #[serde(rename = "returnSecureToken")]
    return_secure_token: bool,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    message: String,
}

impl FirebaseAuth {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Sign in with email and password
    pub async fn sign_in_with_email_password(
        &self,
        email: String,
        password: String,
    ) -> Result<AuthResponse, String> {
        let url = format!(
            "{}/accounts:signInWithPassword?key={}",
            FIREBASE_AUTH_API, FIREBASE_API_KEY
        );

        let request = SignInWithPasswordRequest {
            email,
            password,
            return_secure_token: true,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if response.status().is_success() {
            response
                .json::<AuthResponse>()
                .await
                .map_err(|e| format!("Failed to parse response: {}", e))
        } else {
            let error: ErrorResponse = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse error: {}", e))?;
            Err(error.error.message)
        }
    }

    /// Get Google OAuth URL
    pub fn get_google_oauth_url(&self, redirect_uri: &str) -> String {
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?\
            client_id={}&\
            redirect_uri={}&\
            response_type=code&\
            scope=email%20profile&\
            access_type=offline",
            FIREBASE_AUTH_CLIENT_ID,
            urlencoding::encode(redirect_uri)
        )
    }

    /// Exchange Google OAuth code for Firebase token
    pub async fn sign_in_with_google_oauth(
        &self,
        id_token: String,
        redirect_uri: String,
    ) -> Result<AuthResponse, String> {
        let url = format!(
            "{}/accounts:signInWithIdp?key={}",
            FIREBASE_AUTH_API, FIREBASE_API_KEY
        );

        let post_body = format!("id_token={}&providerId=google.com", id_token);

        let request = SignInWithIdpRequest {
            post_body,
            request_uri: redirect_uri,
            return_secure_token: true,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if response.status().is_success() {
            response
                .json::<AuthResponse>()
                .await
                .map_err(|e| format!("Failed to parse response: {}", e))
        } else {
            let error: ErrorResponse = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse error: {}", e))?;
            Err(error.error.message)
        }
    }
}

/// Start a temporary HTTP server to receive OAuth callback
pub fn start_oauth_callback_server() -> Result<(String, std::sync::mpsc::Receiver<String>), String>
{
    let (tx, rx) = std::sync::mpsc::channel();

    let port: u16 = 50325;
    let server = tiny_http::Server::http(format!("127.0.0.1:{}", port))
        .map_err(|e| format!("Failed to start server: {}", e))?;

    let redirect_uri = format!("http://127.0.0.1:{}/callback", port);

    std::thread::spawn(move || {
        if let Ok(request) = server.recv() {
            let url = request.url();

            // Parse the query string to get the code or id_token
            if let Some(query_start) = url.find('?') {
                let query = &url[query_start + 1..];
                for param in query.split('&') {
                    if let Some((key, value)) = param.split_once('=') {
                        if key == "code" || key == "id_token" {
                            let _ = tx.send(value.to_string());

                            // Send success response
                            let response = tiny_http::Response::from_string(
                                "Authentication successful! You can close this window.",
                            );
                            let _ = request.respond(response);
                            return;
                        }
                    }
                }
            }

            // Send error response
            let response = tiny_http::Response::from_string("Authentication failed or cancelled.")
                .with_status_code(400);
            let _ = request.respond(response);
        }
    });

    Ok((redirect_uri, rx))
}
