use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const FIREBASE_PROJECT_ID: &str = "heek-chat";
const FIREBASE_PUBLIC_KEYS_URL: &str =
    "https://www.googleapis.com/robot/v1/metadata/x509/securetoken@system.gserviceaccount.com";

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TokenPayload {
    pub email: Option<String>,
    pub iss: String,
    pub aud: String,
    pub exp: u64,
    pub iat: u64,
    pub sub: String,
}

async fn fetch_public_keys() -> Result<HashMap<String, String>, String> {
    let client = reqwest::Client::new();
    let response = client
        .get(FIREBASE_PUBLIC_KEYS_URL)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch public keys: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch public keys: {}",
            response.status()
        ));
    }

    let keys: HashMap<String, String> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse public keys: {}", e))?;

    Ok(keys)
}

pub async fn verify_firebase_token(id_token: &str) -> Result<String, String> {
    let header =
        decode_header(id_token).map_err(|e| format!("Failed to decode token header: {}", e))?;
    let kid = header
        .kid
        .ok_or_else(|| "Token missing key ID".to_string())?;

    let public_keys = fetch_public_keys().await?;
    let public_key_pem = public_keys
        .get(&kid)
        .ok_or_else(|| "Public key not found for token".to_string())?;
    let decoding_key = DecodingKey::from_rsa_pem(public_key_pem.as_bytes())
        .map_err(|e| format!("Failed to create decoding key: {}", e))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[FIREBASE_PROJECT_ID]);
    validation.set_issuer(&[format!(
        "https://securetoken.google.com/{}",
        FIREBASE_PROJECT_ID
    )]);
    let token_data = decode::<TokenPayload>(id_token, &decoding_key, &validation)
        .map_err(|e| format!("Token verification failed: {}", e))?;

    // Additional checks
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if token_data.claims.exp < now {
        return Err("Token expired".to_string());
    }
    if token_data.claims.iat > now {
        return Err("Token issued in the future".to_string());
    }

    Ok(token_data.claims.sub)
}
