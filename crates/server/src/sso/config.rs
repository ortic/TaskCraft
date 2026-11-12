//! Sign-in configuration (flattened into [`crate::config::Config`] as `config.sso`).

use std::path::PathBuf;

use clap::Args;

/// External sign-in providers and the local-password switch (CLI flags or `TASKCRAFT_*`
/// environment variables).
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Sign-in (SSO, LDAP, SAML)")]
pub struct SsoConfig {
    /// External sign-in providers as a JSON list (OIDC, GitHub, LDAP, SAML; see README).
    #[arg(long, env = "TASKCRAFT_AUTH_PROVIDERS", hide_env_values = true)]
    pub auth_providers: Option<String>,

    /// File with the provider list (JSON), e.g. a mounted Kubernetes secret.
    #[arg(long, env = "TASKCRAFT_AUTH_PROVIDERS_FILE")]
    pub auth_providers_file: Option<PathBuf>,

    /// Turn off local passwords: people sign in through a provider (LDAP accounts still use the
    /// password form). The first-run setup still asks for the owner's password.
    #[arg(long, env = "TASKCRAFT_DISABLE_PASSWORD_LOGIN", default_value_t = false)]
    pub disable_password_login: bool,

    /// OpenID Connect issuer of a single provider (shortcut for one `oidc` entry with id `oidc`),
    /// e.g. https://keycloak.example.com/realms/acme
    #[arg(long, env = "TASKCRAFT_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,

    /// Client id at that issuer.
    #[arg(long, env = "TASKCRAFT_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,

    /// Client secret at that issuer.
    #[arg(long, env = "TASKCRAFT_OIDC_CLIENT_SECRET", hide_env_values = true)]
    pub oidc_client_secret: Option<String>,

    /// Label of its button on the login page (default "Single sign-on").
    #[arg(long, env = "TASKCRAFT_OIDC_NAME")]
    pub oidc_name: Option<String>,

    /// Only these e-mail domains may sign in through it (comma-separated).
    #[arg(long, env = "TASKCRAFT_OIDC_ALLOWED_DOMAINS")]
    pub oidc_allowed_domains: Option<String>,

    /// Create accounts for unknown people on their first sign-in (default: true).
    #[arg(long, env = "TASKCRAFT_OIDC_AUTO_CREATE")]
    pub oidc_auto_create: Option<bool>,

    /// "Continue with Google": OAuth client id.
    #[arg(long, env = "TASKCRAFT_GOOGLE_CLIENT_ID")]
    pub google_client_id: Option<String>,

    /// Google OAuth client secret.
    #[arg(long, env = "TASKCRAFT_GOOGLE_CLIENT_SECRET", hide_env_values = true)]
    pub google_client_secret: Option<String>,

    /// Google Workspace domains that may sign in (comma-separated); also lets new people in.
    #[arg(long, env = "TASKCRAFT_GOOGLE_ALLOWED_DOMAINS")]
    pub google_allowed_domains: Option<String>,

    /// "Continue with Microsoft" (Entra ID): application (client) id.
    #[arg(long, env = "TASKCRAFT_MICROSOFT_CLIENT_ID")]
    pub microsoft_client_id: Option<String>,

    /// Microsoft client secret.
    #[arg(long, env = "TASKCRAFT_MICROSOFT_CLIENT_SECRET", hide_env_values = true)]
    pub microsoft_client_secret: Option<String>,

    /// Microsoft Entra tenant id (required; multi-tenant `common` isn't supported).
    #[arg(long, env = "TASKCRAFT_MICROSOFT_TENANT")]
    pub microsoft_tenant: Option<String>,

    /// E-mail domains that may sign in with Microsoft (comma-separated).
    #[arg(long, env = "TASKCRAFT_MICROSOFT_ALLOWED_DOMAINS")]
    pub microsoft_allowed_domains: Option<String>,
}
