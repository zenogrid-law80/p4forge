use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Query, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{COOKIE, LOCATION, SET_COOKIE},
    },
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use url::Url;
use uuid::Uuid;

use crate::error::ApiError;

pub const ALLOWED_DOMAIN: &str = "zenogrid.co.kr";
pub const SESSION_COOKIE: &str = "p4forge_session";
const CSRF_COOKIE: &str = "p4forge_csrf";
const STATE_COOKIE: &str = "p4forge_oauth_state";

#[derive(Clone)]
pub struct AuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub public_url: Url,
    pub bootstrap_admin: String,
}

impl AuthConfig {
    pub fn validate(&self) -> Result<()> {
        let url = &self.public_url;
        ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
            "public URL must be HTTPS or loopback HTTP"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "public URL must be an origin without credentials, path, query or fragment"
        );
        ensure!(
            self.bootstrap_admin.is_empty()
                || self
                    .bootstrap_admin
                    .rsplit_once('@')
                    .is_some_and(|(local, domain)| !local.is_empty()
                        && domain.eq_ignore_ascii_case(ALLOWED_DOMAIN)),
            "bootstrap admin must be a Workspace email"
        );
        ensure!(
            self.client_id.is_empty() == self.client_secret.is_empty(),
            "set both GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET"
        );
        Ok(())
    }
    fn secure(&self) -> bool {
        self.public_url.scheme() == "https"
    }
    fn callback(&self) -> String {
        self.public_url
            .join("auth/google/callback")
            .expect("validated origin")
            .to_string()
    }
}

#[derive(Clone)]
pub struct AuthService {
    pub pool: PgPool,
    pub config: Arc<AuthConfig>,
    client: reqwest::Client,
    keys: Arc<RwLock<Option<KeyCache>>>,
}

struct KeyCache {
    loaded_at: Instant,
    keys: Vec<GoogleKey>,
}

#[derive(Clone, Deserialize)]
struct GoogleKey {
    kid: String,
    kty: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct KeyResponse {
    keys: Vec<GoogleKey>,
}
#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Debug, Deserialize)]
struct GoogleClaims {
    sub: String,
    iat: u64,
    azp: Option<String>,
    aud: serde_json::Value,
    email: String,
    email_verified: bool,
    hd: Option<String>,
    nonce: String,
    name: Option<String>,
}

#[derive(Clone, Debug, Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
}

#[derive(Clone)]
pub struct Session {
    pub user: User,
    pub token_hash: Vec<u8>,
}

#[derive(FromRow)]
struct SessionRow {
    id: Uuid,
    email: String,
    name: String,
    is_admin: bool,
    csrf_hash: Vec<u8>,
}

impl AuthService {
    pub fn new(pool: PgPool, config: AuthConfig) -> Result<Self> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent("p4forge/0.1")
            .build()?;
        Ok(Self {
            pool,
            config: Arc::new(config),
            client,
            keys: Arc::new(RwLock::new(None)),
        })
    }

    pub fn configured(&self) -> bool {
        !self.config.client_id.is_empty()
    }

    async fn keys(&self, refresh: bool) -> Result<Vec<GoogleKey>> {
        if !refresh
            && let Some(cache) = self.keys.read().await.as_ref()
            && cache.loaded_at.elapsed() < Duration::from_secs(3600)
        {
            return Ok(cache.keys.clone());
        }
        let response = self
            .client
            .get("https://www.googleapis.com/oauth2/v3/certs")
            .send()
            .await?
            .error_for_status()?
            .json::<KeyResponse>()
            .await?;
        ensure!(!response.keys.is_empty(), "Google returned empty JWKS");
        *self.keys.write().await = Some(KeyCache {
            loaded_at: Instant::now(),
            keys: response.keys.clone(),
        });
        Ok(response.keys)
    }

    async fn verify(&self, token: &str, nonce: &str) -> Result<GoogleClaims> {
        let header = decode_header(token)?;
        ensure!(
            header.alg == Algorithm::RS256,
            "Google token must use RS256"
        );
        let kid = header.kid.context("Google token has no key ID")?;
        for refresh in [false, true] {
            let keys = self.keys(refresh).await?;
            if let Some(key) = keys.iter().find(|key| key.kid == kid) {
                ensure!(
                    key.kty == "RSA"
                        && key.alg.as_deref().is_none_or(|v| v == "RS256")
                        && key.usage.as_deref().is_none_or(|v| v == "sig"),
                    "invalid Google signing key"
                );
                let mut validation = Validation::new(Algorithm::RS256);
                validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
                validation.set_audience(&[&self.config.client_id]);
                validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
                let claims = decode::<GoogleClaims>(
                    token,
                    &DecodingKey::from_rsa_components(&key.n, &key.e)?,
                    &validation,
                )?
                .claims;
                validate_claims(
                    &claims,
                    &self.config.client_id,
                    nonce,
                    SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
                )?;
                return Ok(claims);
            }
        }
        anyhow::bail!("Google signing key not found")
    }

    pub async fn authenticate(
        &self,
        method: &Method,
        headers: &HeaderMap,
    ) -> Result<Session, ApiError> {
        let token = cookie(headers, SESSION_COOKIE)
            .ok_or_else(|| ApiError::unauthorized("Google Workspace login required"))?;
        let row: SessionRow = sqlx::query_as("SELECT u.id,u.email,u.name,u.is_admin,s.csrf_hash FROM auth_sessions s JOIN users u ON u.id=s.user_id WHERE s.token_hash=$1 AND s.expires_at>now() AND u.active")
            .bind(token_hash(&token)).fetch_optional(&self.pool).await?.ok_or_else(|| ApiError::unauthorized("session is expired or revoked"))?;
        if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
            let browser_token = cookie(headers, CSRF_COOKIE)
                .ok_or_else(|| ApiError::forbidden("missing CSRF cookie"))?;
            let header_token = headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ApiError::forbidden("missing CSRF header"))?;
            if !bool::from(browser_token.as_bytes().ct_eq(header_token.as_bytes()))
                || !bool::from(token_hash(header_token).ct_eq(&row.csrf_hash))
            {
                return Err(ApiError::forbidden("CSRF token mismatch"));
            }
        }
        Ok(Session {
            user: User {
                id: row.id,
                email: row.email,
                name: row.name,
                is_admin: row.is_admin,
            },
            token_hash: token_hash(&token),
        })
    }
}

fn validate_claims(claims: &GoogleClaims, client_id: &str, nonce: &str, now: u64) -> Result<()> {
    ensure!(
        !claims.sub.is_empty() && claims.sub.len() <= 255,
        "invalid Google subject"
    );
    ensure!(claims.email_verified, "Google email is not verified");
    ensure!(
        claims
            .hd
            .as_deref()
            .is_some_and(|hd| hd.eq_ignore_ascii_case(ALLOWED_DOMAIN)),
        "Workspace domain is not allowed"
    );
    ensure!(
        claims.email.len() <= 255
            && claims
                .email
                .rsplit_once('@')
                .is_some_and(|(name, domain)| !name.is_empty()
                    && domain.eq_ignore_ascii_case(ALLOWED_DOMAIN)),
        "email domain is not allowed"
    );
    ensure!(
        bool::from(claims.nonce.as_bytes().ct_eq(nonce.as_bytes())),
        "nonce mismatch"
    );
    ensure!(
        claims.iat <= now.saturating_add(60),
        "token issued in the future"
    );
    ensure!(
        claims.azp.as_deref().is_none_or(|value| value == client_id),
        "authorized party mismatch"
    );
    if claims.aud.as_array().is_some_and(|values| values.len() > 1) {
        ensure!(
            claims.azp.as_deref() == Some(client_id),
            "multiple audiences require authorized party"
        );
    }
    Ok(())
}

pub async fn login(State(auth): State<AuthService>) -> Result<Response, ApiError> {
    if !auth.configured() {
        return Err(ApiError::unavailable(
            "configure Google Workspace OAuth client ID and secret",
        ));
    }
    let state = random_token();
    let nonce = random_token();
    let verifier = random_token();
    sqlx::query("DELETE FROM oauth_states WHERE expires_at<=now()")
        .execute(&auth.pool)
        .await?;
    sqlx::query("DELETE FROM auth_sessions WHERE expires_at<=now()")
        .execute(&auth.pool)
        .await?;
    sqlx::query("INSERT INTO oauth_states(state_hash,nonce,verifier,expires_at) VALUES($1,$2,$3,now()+interval '10 minutes')").bind(token_hash(&state)).bind(&nonce).bind(&verifier).execute(&auth.pool).await?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut destination =
        Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("static URL");
    destination
        .query_pairs_mut()
        .append_pair("client_id", &auth.config.client_id)
        .append_pair("redirect_uri", &auth.config.callback())
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("hd", ALLOWED_DOMAIN)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(destination.as_str()).map_err(ApiError::internal)?,
    );
    response.headers_mut().append(
        SET_COOKIE,
        set_cookie(
            STATE_COOKIE,
            &state,
            "/auth/google/callback",
            600,
            auth.config.secure(),
            true,
        ),
    );
    Ok(response)
}

#[derive(Deserialize)]
pub struct Callback {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

pub async fn callback(
    State(auth): State<AuthService>,
    headers: HeaderMap,
    Query(input): Query<Callback>,
) -> Result<Response, ApiError> {
    let browser_state = cookie(&headers, STATE_COOKIE)
        .ok_or_else(|| ApiError::bad_request("missing login state cookie"))?;
    if !bool::from(browser_state.as_bytes().ct_eq(input.state.as_bytes())) {
        return Err(ApiError::bad_request("login state mismatch"));
    }
    let (nonce, verifier): (String, String) = sqlx::query_as("DELETE FROM oauth_states WHERE state_hash=$1 AND expires_at>now() RETURNING nonce,verifier").bind(token_hash(&input.state)).fetch_optional(&auth.pool).await?.ok_or_else(|| ApiError::bad_request("login state expired or already used"))?;
    if input.error.is_some() {
        return Err(ApiError::unauthorized("Google login was not completed"));
    }
    let code = input
        .code
        .ok_or_else(|| ApiError::bad_request("missing authorization code"))?;
    let callback_url = auth.config.callback();
    let token = auth
        .client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code.as_str()),
            ("client_id", auth.config.client_id.as_str()),
            ("client_secret", auth.config.client_secret.as_str()),
            ("redirect_uri", callback_url.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|_| ApiError::unavailable("Google authentication is unavailable"))?
        .error_for_status()
        .map_err(|_| ApiError::unauthorized("Google rejected the authorization code"))?
        .json::<TokenResponse>()
        .await
        .map_err(|_| ApiError::unavailable("Google authentication is unavailable"))?;
    let claims = auth
        .verify(&token.id_token, &nonce)
        .await
        .map_err(|_| ApiError::unauthorized("Google Workspace identity verification failed"))?;
    let mut tx = auth.pool.begin().await?;
    let is_admin = !auth.config.bootstrap_admin.is_empty()
        && claims
            .email
            .eq_ignore_ascii_case(&auth.config.bootstrap_admin);
    let user: User = sqlx::query_as("INSERT INTO users(id,google_sub,email,name,is_admin) VALUES($1,$2,$3,$4,$5) ON CONFLICT(google_sub) DO UPDATE SET email=EXCLUDED.email,name=EXCLUDED.name RETURNING id,email,name,is_admin")
        .bind(Uuid::new_v4()).bind(&claims.sub).bind(&claims.email).bind(claims.name.as_deref().unwrap_or(&claims.email)).bind(is_admin).fetch_one(&mut *tx).await?;
    let active: bool = sqlx::query_scalar("SELECT active FROM users WHERE id=$1")
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    if !active {
        return Err(ApiError::forbidden("account is disabled"));
    }
    let session = random_token();
    let csrf = random_token();
    sqlx::query("INSERT INTO auth_sessions(token_hash,user_id,csrf_hash,expires_at) VALUES($1,$2,$3,now()+interval '8 hours')").bind(token_hash(&session)).bind(user.id).bind(token_hash(&csrf)).execute(&mut *tx).await?;
    tx.commit().await?;
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(LOCATION, HeaderValue::from_static("/"));
    response.headers_mut().append(
        SET_COOKIE,
        set_cookie(
            SESSION_COOKIE,
            &session,
            "/",
            28800,
            auth.config.secure(),
            true,
        ),
    );
    response.headers_mut().append(
        SET_COOKIE,
        set_cookie(CSRF_COOKIE, &csrf, "/", 28800, auth.config.secure(), false),
    );
    response.headers_mut().append(
        SET_COOKIE,
        set_cookie(
            STATE_COOKIE,
            "",
            "/auth/google/callback",
            0,
            auth.config.secure(),
            true,
        ),
    );
    Ok(response)
}

pub async fn logout(
    State(auth): State<AuthService>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session = auth.authenticate(&Method::POST, &headers).await?;
    sqlx::query("DELETE FROM auth_sessions WHERE token_hash=$1")
        .bind(session.token_hash)
        .execute(&auth.pool)
        .await?;
    let mut response = Json(serde_json::json!({"ok":true})).into_response();
    for (cookie, http_only) in [(SESSION_COOKIE, true), (CSRF_COOKIE, false)] {
        response.headers_mut().append(
            SET_COOKIE,
            set_cookie(cookie, "", "/", 0, auth.config.secure(), http_only),
        );
    }
    Ok(response)
}

pub fn random_token() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}
fn set_cookie(
    name: &str,
    value: &str,
    path: &str,
    age: i64,
    secure: bool,
    http_only: bool,
) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{name}={value}; Path={path}; Max-Age={age}; SameSite=Lax{}{}",
        if secure { "; Secure" } else { "" },
        if http_only { "; HttpOnly" } else { "" }
    ))
    .expect("generated cookie values")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn claims() -> GoogleClaims {
        serde_json::from_value(serde_json::json!({"sub":"123","iat":1000,"aud":"client","email":"developer@zenogrid.co.kr","email_verified":true,"hd":"zenogrid.co.kr","nonce":"nonce"})).unwrap()
    }
    #[test]
    fn requires_verified_workspace_nonce_and_authorized_party() {
        assert!(validate_claims(&claims(), "client", "nonce", 1000).is_ok());
        for mutate in [0, 1, 2, 3, 4, 5] {
            let mut claim = claims();
            match mutate {
                0 => claim.email_verified = false,
                1 => claim.hd = None,
                2 => claim.email = "user@gmail.com".into(),
                3 => claim.nonce = "wrong".into(),
                4 => claim.azp = Some("other-client".into()),
                _ => claim.iat = 2000,
            }
            assert!(validate_claims(&claim, "client", "nonce", 1000).is_err());
        }
        let mut claim = claims();
        claim.aud = serde_json::json!(["client", "another"]);
        assert!(validate_claims(&claim, "client", "nonce", 1000).is_err());
    }
    #[test]
    fn http_is_only_allowed_for_loopback() {
        for (url, valid) in [
            ("http://127.0.0.1:8080/", true),
            ("https://ci.example.com/", true),
            ("http://ci.example.com/", false),
            ("https://ci.example.com/prefix/", false),
            ("https://user:secret@ci.example.com/", false),
        ] {
            let config = AuthConfig {
                client_id: "".into(),
                client_secret: "".into(),
                public_url: Url::parse(url).unwrap(),
                bootstrap_admin: "".into(),
            };
            assert_eq!(config.validate().is_ok(), valid);
        }
    }

    #[tokio::test]
    async fn verifies_signatures_and_rejects_invalid_oidc_tokens_offline() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@localhost/unused")
            .unwrap();
        let auth = AuthService::new(
            pool,
            AuthConfig {
                client_id: "client".into(),
                client_secret: "test-only".into(),
                public_url: "https://p4forge.zenogrid.co.kr:8444/".parse().unwrap(),
                bootstrap_admin: String::new(),
            },
        )
        .unwrap();
        let jwks: KeyResponse =
            serde_json::from_str(include_str!("../tests/fixtures/oidc-test-jwks.json")).unwrap();
        *auth.keys.write().await = Some(KeyCache {
            loaded_at: Instant::now(),
            keys: jwks.keys,
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!(
            "../tests/fixtures/oidc-test-key.pem"
        ))
        .unwrap();
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some("test-only".into());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let valid = serde_json::json!({"sub":"test-user","iat":now,"exp":now+300,"iss":"https://accounts.google.com","aud":"client","email":"test@zenogrid.co.kr","email_verified":true,"hd":"zenogrid.co.kr","nonce":"bound-nonce"});
        let token = jsonwebtoken::encode(&header, &valid, &key).unwrap();
        assert!(auth.verify(&token, "bound-nonce").await.is_ok());
        for (field, value) in [
            ("exp", serde_json::json!(now - 120)),
            ("iss", serde_json::json!("https://evil.example")),
            ("aud", serde_json::json!("another-client")),
            ("nonce", serde_json::json!("replayed-nonce")),
            ("email_verified", serde_json::json!(false)),
            ("hd", serde_json::json!("another.example")),
            ("azp", serde_json::json!("another-client")),
            ("iat", serde_json::json!(now + 3600)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(
                auth.verify(
                    &jsonwebtoken::encode(&header, &invalid, &key).unwrap(),
                    "bound-nonce"
                )
                .await
                .is_err(),
                "accepted invalid {field}"
            );
        }
        let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
        let mut signature = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
        signature[0] ^= 1;
        parts[2] = URL_SAFE_NO_PAD.encode(signature);
        assert!(auth.verify(&parts.join("."), "bound-nonce").await.is_err());
        let hs = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &valid,
            &jsonwebtoken::EncodingKey::from_secret(b"test-only"),
        )
        .unwrap();
        assert!(auth.verify(&hs, "bound-nonce").await.is_err());
    }
}
