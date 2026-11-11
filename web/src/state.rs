use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use oauth2::basic::BasicClient;
use oauth2::url::ParseError;
use oauth2::{
    AuthUrl,
    ClientId,
    ClientSecret,
    EndpointNotSet,
    EndpointSet,
    RedirectUrl,
    TokenUrl,
};
use patreon::oauth::PatreonApp;
use secrecy::{ExposeSecret, SecretString};
use twilight_http::Client;
use twilight_model::user::CurrentUserGuild;
use youtube::{YoutubeApp, YoutubeRuntime};
use zayden_app::applications::Slot;
use zayden_app::config::BotConfig;
use zayden_app::state::AppState as ZaydenAppState;

use crate::auth::SessionUser;
use crate::error::StartupError;
use crate::rest::RestClients;

const DISCORD_OAUTH_AUTH_URL: &str = "https://discord.com/oauth2/authorize";
const DISCORD_OAUTH_TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const CACHE_CAPACITY: u64 = 1024;
const CACHE_TTL: Duration = Duration::from_mins(1);
const USERS_CACHE_TTL: Duration = Duration::from_mins(10);

pub type DiscordOAuthClient = BasicClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
>;

#[derive(Clone)]
pub struct SessionIdentity {
    pub user_id: i64,
    pub access_token: String,
}

pub type SessionCache = Cache<String, SessionIdentity>;
pub type UserGuildsCache = Cache<i64, Arc<[CurrentUserGuild]>>;
pub type SessionUsersCache = Cache<i64, SessionUser>;
pub type MemberNamesCache = Cache<(u64, u64), String>;

pub struct OAuthState {
    pub client: DiscordOAuthClient,
    pub http: oauth2::reqwest::Client,
}

pub struct DiscordState {
    pub http: Arc<Client>,
    pub user_guilds: UserGuildsCache,
    pub users: SessionUsersCache,
    pub member_names: MemberNamesCache,
}

pub struct IntegrationsState {
    pub patreon: Option<PatreonApp>,
    pub patreon_webhook_uri: String,
    pub youtube: Option<YoutubeApp>,
    pub youtube_runtime: Option<YoutubeRuntime>,
    pub kofi_verification_token: Option<SecretString>,
}

pub struct SiteUrls {
    pub invite: Option<String>,
    pub upgrade: Option<String>,
    pub dashboard: String,
}

pub struct WebState {
    pub app: Arc<ZaydenAppState>,
    pub sessions: SessionCache,
    pub oauth: OAuthState,
    pub discord: DiscordState,
    pub rest: RestClients,
    pub integrations: IntegrationsState,
    pub urls: SiteUrls,
}

impl WebState {
    pub fn new(
        app: Arc<ZaydenAppState>,
        config: &BotConfig,
    ) -> Result<Self, StartupError> {
        let client_secret = config
            .discord_client_secret
            .as_ref()
            .ok_or(StartupError::MissingConfig("DISCORD_CLIENT_SECRET"))?;
        let rest =
            RestClients::new(app.keyring.clone(), RestClients::default_builder());
        Ok(Self {
            app,
            rest,
            sessions: cache(CACHE_TTL),
            oauth: OAuthState {
                client: build_oauth_client(config, client_secret)?,
                http: oauth2::reqwest::Client::new(),
            },
            discord: DiscordState {
                http: Arc::new(Client::new(
                    config.discord_token.expose_secret().to_owned(),
                )),
                user_guilds: cache(CACHE_TTL),
                users: cache(USERS_CACHE_TTL),
                member_names: cache(USERS_CACHE_TTL),
            },
            integrations: IntegrationsState {
                patreon: config.patreon.as_ref().map(|p| PatreonApp {
                    client_id: p.client_id.clone(),
                    client_secret: p.client_secret.expose_secret().to_owned(),
                    redirect_uri: p.redirect_uri.clone(),
                }),
                // Patreon delivers to a fixed URL, so it is derived from the
                // callback the app is already registered against.
                patreon_webhook_uri: config
                    .patreon
                    .as_ref()
                    .map_or_else(String::new, |p| {
                        patreon_webhook_uri(&p.redirect_uri)
                    }),
                youtube: config.youtube.as_ref().map(|y| YoutubeApp {
                    client_id: y.client_id.clone(),
                    client_secret: y.client_secret.expose_secret().to_owned(),
                    redirect_uri: y.redirect_uri.clone(),
                }),
                youtube_runtime: config.youtube.as_ref().and_then(|y| {
                    let Some(key) = config.google_api_key.as_ref() else {
                        tracing::warn!(
                            "youtube is configured but GOOGLE_API_KEY is not set; youtube is disabled"
                        );
                        return None;
                    };
                    Some(YoutubeRuntime {
                        api_key: Arc::from(key.expose_secret()),
                        webhook_uri: Arc::from(y.webhook_uri.as_str()),
                    })
                }),
                kofi_verification_token: config.kofi_verification_token.clone(),
            },
            urls: SiteUrls {
                invite: config.invite_url.clone(),
                upgrade: config.upgrade_url.clone(),
                dashboard: config.dashboard_url.clone(),
            },
        })
    }

    pub async fn app_client(&self, application_id: i64) -> Option<Arc<Client>> {
        if application_id == self.app.zayden_id.cast_signed() {
            return Some(Arc::clone(&self.discord.http));
        }

        self.rest.client(&self.app.db, application_id).await
    }

    pub async fn guild_client(&self, guild_id: i64) -> Arc<Client> {
        match self.app.serving.fetch(guild_id, Slot::Zayden).await {
            Ok(Some(bot)) if bot.present => {
                if let Some(client) =
                    self.app_client(bot.application_id.cast_signed()).await
                {
                    return client;
                }
            },
            Ok(_) => {},
            Err(e) => {
                tracing::warn!(guild_id, error = %e, "failed to resolve the serving bot");
            },
        }

        match crate::guild::seat::trusted_in_guild(&self.app, guild_id).await {
            Ok(present) => {
                for profile in present {
                    if let Some(client) =
                        self.app_client(profile.application_id).await
                    {
                        return client;
                    }
                }
            },
            Err(e) => {
                tracing::warn!(guild_id, error = %e, "failed to list the bots in the guild");
            },
        }

        Arc::clone(&self.discord.http)
    }
}

#[must_use]
pub fn patreon_webhook_uri(redirect_uri: &str) -> String {
    if !redirect_uri.contains("/patreon/callback") {
        tracing::warn!(
            redirect_uri,
            "Patreon redirect URI has no /patreon/callback path; the webhook URI \
             derived from it will not reach /webhooks/patreon"
        );
    }

    redirect_uri.replace("/patreon/callback", "/webhooks/patreon")
}

fn cache<K, V>(ttl: Duration) -> Cache<K, V>
where
    K: Send + Sync + Eq + std::hash::Hash + 'static,
    V: Send + Sync + Clone + 'static,
{
    Cache::builder().max_capacity(CACHE_CAPACITY).time_to_live(ttl).build()
}

fn build_oauth_client(
    config: &BotConfig,
    secret: &SecretString,
) -> Result<DiscordOAuthClient, ParseError> {
    Ok(BasicClient::new(ClientId::new(config.zayden_id.to_string()))
        .set_client_secret(ClientSecret::new(secret.expose_secret().to_owned()))
        .set_auth_uri(AuthUrl::new(DISCORD_OAUTH_AUTH_URL.to_string())?)
        .set_token_uri(TokenUrl::new(DISCORD_OAUTH_TOKEN_URL.to_string())?)
        .set_redirect_uri(RedirectUrl::new(config.redirect_uri.clone())?))
}
