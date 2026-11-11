use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use sqlx::PgPool;
use twilight_http::Client;
use zayden_app::applications::{ApplicationError, ApplicationStore, Keyring};

pub type ClientBuilder = Arc<dyn Fn(String) -> Client + Send + Sync>;

pub const CLIENT_TTL: Duration = Duration::from_secs(600);

const CACHE_CAPACITY: u64 = 1024;

pub struct RestClients {
    keyring: Option<Arc<Keyring>>,
    build: ClientBuilder,
    cache: moka::future::Cache<(i64, i16, Vec<u8>), Arc<Client>>,
}

impl RestClients {
    #[must_use]
    pub fn new(keyring: Option<Arc<Keyring>>, build: ClientBuilder) -> Self {
        Self {
            keyring,
            build,
            cache: moka::future::Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_live(CLIENT_TTL)
                .build(),
        }
    }

    #[must_use]
    pub fn default_builder() -> ClientBuilder {
        Arc::new(Client::new)
    }

    pub async fn client(
        &self,
        pool: &PgPool,
        application_id: i64,
    ) -> Option<Arc<Client>> {
        let keyring = self.keyring.as_ref()?;

        let sealed = match ApplicationStore::sealed_token(pool, application_id).await
        {
            Ok(Some(sealed)) => sealed,
            Ok(None) => {
                tracing::debug!(
                    application_id,
                    "the application has no stored token"
                );
                return None;
            },
            Err(e) => {
                tracing::warn!(
                    application_id,
                    error = %e,
                    "failed to read the application's sealed token"
                );
                return None;
            },
        };

        let key = (application_id, sealed.key_id, sealed.nonce.clone());
        let opened = self
            .cache
            .try_get_with(key, async {
                let token = keyring.open(application_id, &sealed)?;
                Ok::<_, ApplicationError>(Arc::new((self.build)(
                    token.expose_secret().to_owned(),
                )))
            })
            .await;

        match opened {
            Ok(client) => Some(client),
            Err(e) => {
                tracing::warn!(
                    application_id,
                    error = %e,
                    "failed to open the application's token"
                );
                None
            },
        }
    }
}
