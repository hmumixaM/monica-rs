//! Read-only mail authorization. Credentials never leave the encrypted store.
use crate::credentials::CredentialCipher;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::time::Duration;
use uuid::Uuid;

pub const CALLBACK: &str = "/api/v1/mail-oauth/callback";
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn random() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    gmail_token: String,
    outlook_token: String,
    gmail_identity: String,
    outlook_identity: String,
}
impl OAuthClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            gmail_token: "https://oauth2.googleapis.com/token".into(),
            outlook_token: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            gmail_identity: "https://gmail.googleapis.com/gmail/v1/users/me/profile".into(),
            outlook_identity:
                "https://graph.microsoft.com/v1.0/me?$select=mail,userPrincipalName,displayName"
                    .into(),
        })
    }
    fn endpoint(&self, provider: &str) -> Result<&str> {
        match provider {
            "gmail" => Ok(&self.gmail_token),
            "outlook" => Ok(&self.outlook_token),
            _ => anyhow::bail!("unsupported OAuth provider"),
        }
    }
    async fn response(response: reqwest::Response) -> Result<Value> {
        // Never include token responses or provider error bodies in errors/logs.
        ensure!(
            response.status().is_success(),
            "mail authorization request failed"
        );
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 256 * 1024,
                "mail authorization response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).context("invalid mail authorization response")
    }
    async fn exchange(&self, provider: &str, fields: &[(&str, String)]) -> Result<Value> {
        Self::response(
            self.http
                .post(self.endpoint(provider)?)
                .form(fields)
                .send()
                .await?,
        )
        .await
    }
    pub async fn complete(
        &self,
        pool: &PgPool,
        cipher: &CredentialCipher,
        session: &str,
        state: &str,
        code: &str,
        redirect: &str,
    ) -> Result<Uuid> {
        // DELETE is committed before network IO: a failed exchange cannot replay state.
        let row=sqlx::query("DELETE FROM crm.mail_oauth_requests WHERE state_hash=$1 AND session_hash=$2 AND expires_at>now() RETURNING provider,credential_ciphertext")
            .bind(hash(state)).bind(hash(session)).fetch_optional(pool).await?.context("authorization state expired or belongs to another session")?;
        let provider: String = row.try_get("provider")?;
        let encrypted: String = row.try_get("credential_ciphertext")?;
        let mut credential = cipher.decrypt(&encrypted)?;
        let mut fields = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", redirect.to_string()),
            ("client_id", required(&credential, "client_id")?),
            ("code_verifier", required(&credential, "verifier")?),
        ];
        if let Some(secret) = credential["client_secret"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            fields.push(("client_secret", secret.to_string()));
        }
        let tokens = self.exchange(&provider, &fields).await?;
        update_tokens(&mut credential, &tokens)?;
        credential
            .as_object_mut()
            .context("invalid credentials")?
            .remove("verifier");
        let identity_url = if provider == "gmail" {
            &self.gmail_identity
        } else {
            &self.outlook_identity
        };
        let identity = Self::response(
            self.http
                .get(identity_url)
                .bearer_auth(required(&credential, "access_token")?)
                .send()
                .await?,
        )
        .await?;
        let account = if provider == "gmail" {
            identity["emailAddress"].as_str()
        } else {
            identity["mail"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or(identity["userPrincipalName"].as_str())
        }
        .context("mail identity missing")?;
        let account = crate::connections::normalize_identifier("email", account)
            .context("invalid mail identity")?;
        ensure!(
            !account.is_empty() && account.len() <= 320,
            "invalid mail identity"
        );
        let id = Uuid::new_v5(
            &Uuid::parse_str("a874aa7b-0650-4950-ac70-f979c6b020eb")?,
            format!("mail:{account}").as_bytes(),
        );
        let name = identity["displayName"].as_str().unwrap_or(&account);
        let settings = if provider == "gmail" {
            json!({"folders":["all"]})
        } else {
            json!({"discover_folders":true})
        };
        let mut tx = pool.begin().await?;
        crate::connections::lock_connection(&mut tx, id).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("identifier:email:{account}"))
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,settings,credential_ciphertext,enabled,state) VALUES($1,'mail',$2,$3,$4,$5,$6,true,'connecting') ON CONFLICT(id) DO UPDATE SET display_name=EXCLUDED.display_name,connection_method=EXCLUDED.connection_method,settings=EXCLUDED.settings,credential_ciphertext=EXCLUDED.credential_ciphertext,enabled=true,state='connecting',error=NULL,next_sync_at=now(),updated_at=now(),generation=crm.source_connections.generation+1,lease_owner=NULL,lease_token=NULL,lease_until=NULL")
            .bind(id).bind(&account).bind(name).bind(provider).bind(settings).bind(cipher.encrypt(&credential)?).execute(&mut *tx).await?;
        crate::connections::queue_identifier(&mut tx, "email", &account).await?;
        tx.commit().await?;
        Ok(id)
    }
    /// Refresh only while this worker owns the connection lease. Revocation,
    /// pause, and reconnect fence the result even if they race the HTTP request.
    pub async fn access_token(
        &self,
        pool: &PgPool,
        cipher: &CredentialCipher,
        id: Uuid,
        lease: Uuid,
        force: bool,
    ) -> Result<String> {
        let row=sqlx::query("SELECT connection_method,credential_ciphertext,generation FROM crm.source_connections WHERE id=$1 AND enabled AND lease_token=$2 AND lease_until>now()")
            .bind(id).bind(lease).fetch_optional(pool).await?.context("mail connection lease lost")?;
        let provider: String = row.try_get("connection_method")?;
        let old: String = row.try_get("credential_ciphertext")?;
        let generation: i64 = row.try_get("generation")?;
        let mut credential = cipher.decrypt(&old)?;
        if !force
            && credential["expires_at"].as_i64().unwrap_or(0) > chrono::Utc::now().timestamp() + 90
        {
            return required(&credential, "access_token");
        }
        let mut fields = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", required(&credential, "refresh_token")?),
            ("client_id", required(&credential, "client_id")?),
        ];
        if let Some(secret) = credential["client_secret"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            fields.push(("client_secret", secret.to_string()));
        }
        let tokens = self.exchange(&provider, &fields).await?;
        update_tokens(&mut credential, &tokens)?;
        let changed=sqlx::query("UPDATE crm.source_connections SET credential_ciphertext=$1,updated_at=now() WHERE id=$2 AND credential_ciphertext=$3 AND generation=$4 AND enabled AND lease_token=$5 AND lease_until>now()")
            .bind(cipher.encrypt(&credential)?).bind(id).bind(old).bind(generation).bind(lease).execute(pool).await?.rows_affected();
        ensure!(
            changed == 1,
            "mail connection ownership changed during token refresh"
        );
        required(&credential, "access_token")
    }
}
fn required(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .context("required mail credential missing")
}
fn update_tokens(credential: &mut Value, tokens: &Value) -> Result<()> {
    let access = required(tokens, "access_token")?;
    let expires = tokens["expires_in"]
        .as_i64()
        .or_else(|| tokens["expires_in"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(3600)
        .clamp(0, 86400);
    credential["access_token"] = json!(access);
    credential["expires_at"] = json!(chrono::Utc::now().timestamp() + expires);
    if let Some(refresh) = tokens["refresh_token"].as_str().filter(|s| !s.is_empty()) {
        credential["refresh_token"] = json!(refresh);
    }
    Ok(())
}
pub async fn start(
    pool: &PgPool,
    cipher: &CredentialCipher,
    session: &str,
    provider: &str,
    client_id: &str,
    client_secret: Option<&str>,
    redirect: &str,
) -> Result<String> {
    let (url, scope) = match provider {
        "gmail" => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://www.googleapis.com/auth/gmail.readonly",
        ),
        "outlook" => (
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "offline_access Mail.Read User.Read",
        ),
        _ => anyhow::bail!("unsupported OAuth provider"),
    };
    ensure!(
        !client_id.trim().is_empty() && client_id.len() <= 256,
        "invalid OAuth client id"
    );
    let state = random();
    let verifier = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let credential =
        json!({"client_id":client_id,"client_secret":client_secret,"verifier":verifier});
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM crm.mail_oauth_requests WHERE expires_at<=now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO crm.mail_oauth_requests(state_hash,session_hash,provider,credential_ciphertext,expires_at) VALUES($1,$2,$3,$4,now()+interval '10 minutes')")
        .bind(hash(&state)).bind(hash(session)).bind(provider).bind(cipher.encrypt(&credential)?).execute(&mut *tx).await?;
    tx.commit().await?;
    let mut url = url::Url::parse(url)?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("scope", scope),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    if provider == "gmail" {
        url.query_pairs_mut()
            .extend_pairs([("access_type", "offline"), ("prompt", "consent")]);
    } else {
        url.query_pairs_mut()
            .append_pair("prompt", "select_account");
    }
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn server(body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = body.to_string();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 16384];
            let n = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..n]).into_owned();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });
        (format!("http://{address}"), task)
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn pkce_state_is_session_bound_and_consumed_before_exchange(pool: PgPool) {
        let cipher = CredentialCipher::new("synthetic-signing-key-at-least-32bytes").unwrap();
        let url = start(
            &pool,
            &cipher,
            "session-one",
            "gmail",
            "synthetic-client",
            None,
            "http://127.0.0.1/callback",
        )
        .await
        .unwrap();
        let url = url::Url::parse(&url).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(
            pairs["scope"],
            "https://www.googleapis.com/auth/gmail.readonly"
        );
        let encrypted: String =
            sqlx::query_scalar("SELECT credential_ciphertext FROM crm.mail_oauth_requests")
                .fetch_one(&pool)
                .await
                .unwrap();
        let stored = cipher.decrypt(&encrypted).unwrap();
        assert_eq!(
            pairs["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(stored["verifier"].as_str().unwrap()))
        );
        let (endpoint, task) = server(r#"{"error":"synthetic failure"}"#).await;
        let mut client = OAuthClient::new().unwrap();
        client.gmail_token = endpoint;
        assert!(
            client
                .complete(
                    &pool,
                    &cipher,
                    "another-session",
                    &pairs["state"],
                    "code",
                    "http://127.0.0.1/callback"
                )
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.mail_oauth_requests")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert!(
            client
                .complete(
                    &pool,
                    &cipher,
                    "session-one",
                    &pairs["state"],
                    "code",
                    "http://127.0.0.1/callback"
                )
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.mail_oauth_requests")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert!(task.await.unwrap().contains("code_verifier="));
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn refresh_requires_current_lease_and_preserves_refresh_token(pool: PgPool) {
        let cipher = CredentialCipher::new("synthetic-signing-key-at-least-32bytes").unwrap();
        let id = Uuid::new_v4();
        let lease = Uuid::new_v4();
        let encrypted=cipher.encrypt(&json!({"client_id":"synthetic-client","refresh_token":"synthetic-refresh","expires_at":0})).unwrap();
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,credential_ciphertext,enabled,lease_token,lease_until) VALUES($1,'mail','synthetic@example.invalid','Synthetic','gmail',$2,true,$3,now()+interval '1 minute')").bind(id).bind(encrypted).bind(lease).execute(&pool).await.unwrap();
        let (endpoint, task) =
            server(r#"{"access_token":"synthetic-access","expires_in":3600}"#).await;
        let mut client = OAuthClient::new().unwrap();
        client.gmail_token = endpoint;
        assert!(
            client
                .access_token(&pool, &cipher, id, Uuid::new_v4(), false)
                .await
                .is_err()
        );
        assert_eq!(
            client
                .access_token(&pool, &cipher, id, lease, false)
                .await
                .unwrap(),
            "synthetic-access"
        );
        assert!(task.await.unwrap().contains("grant_type=refresh_token"));
        let encrypted: String = sqlx::query_scalar(
            "SELECT credential_ciphertext FROM crm.source_connections WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            cipher.decrypt(&encrypted).unwrap()["refresh_token"],
            "synthetic-refresh"
        );
        assert_eq!(
            client
                .access_token(&pool, &cipher, id, lease, false)
                .await
                .unwrap(),
            "synthetic-access"
        );
        sqlx::query(
            "UPDATE crm.source_connections SET enabled=false,generation=generation+1 WHERE id=$1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            client
                .access_token(&pool, &cipher, id, lease, false)
                .await
                .is_err()
        );
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn reconnect_wins_while_refresh_is_in_flight(pool: PgPool) {
        let cipher = CredentialCipher::new("synthetic-signing-key-at-least-32bytes").unwrap();
        let id = Uuid::new_v4();
        let lease = Uuid::new_v4();
        let old = cipher
            .encrypt(
                &json!({"client_id":"old-client","refresh_token":"old-refresh","expires_at":0}),
            )
            .unwrap();
        let replacement=cipher.encrypt(&json!({"client_id":"new-client","refresh_token":"new-refresh","access_token":"new-access","expires_at":chrono::Utc::now().timestamp()+3600})).unwrap();
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,credential_ciphertext,enabled,lease_token,lease_until) VALUES($1,'mail','synthetic-race@example.invalid','Synthetic race','gmail',$2,true,$3,now()+interval '1 minute')").bind(id).bind(old).bind(lease).execute(&pool).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let db = pool.clone();
        let fresh = replacement.clone();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            sqlx::query("UPDATE crm.source_connections SET credential_ciphertext=$1,generation=generation+1,lease_token=NULL WHERE id=$2").bind(fresh).bind(id).execute(&db).await.unwrap();
            let body = r#"{"access_token":"stale-access","expires_in":3600}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        });
        let mut client = OAuthClient::new().unwrap();
        client.gmail_token = format!("http://{address}");
        assert!(
            client
                .access_token(&pool, &cipher, id, lease, true)
                .await
                .is_err()
        );
        task.await.unwrap();
        let current: String = sqlx::query_scalar(
            "SELECT credential_ciphertext FROM crm.source_connections WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current, replacement);
    }
}
