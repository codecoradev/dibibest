//! Env-driven config. The OIDC feature is OFF unless every required var is set to a real value.

#[derive(Clone, Debug)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub public_url: String,
    pub session_secret: String,
    /// Lowercased; empty = any authenticated identity.
    pub allowed_emails: Vec<String>,
}

const MIN_SECRET: usize = 32;

/// Placeholder values copied from `.env.example` files count as unset.
fn real(v: Option<String>) -> Option<String> {
    let v = v?.trim().to_string();
    let l = v.to_lowercase();
    (!v.is_empty() && !["...", "xxx", "changeme"].contains(&l.as_str())).then_some(v)
}

pub fn parse_allowlist(s: &str) -> Vec<String> {
    s.split(',')
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

impl Oidc {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        Some(Self {
            issuer: real(get("OIDC_ISSUER"))?.trim_end_matches('/').to_string(),
            client_id: real(get("OIDC_CLIENT_ID"))?,
            client_secret: real(get("OIDC_CLIENT_SECRET"))?,
            public_url: real(get("PUBLIC_URL"))?.trim_end_matches('/').to_string(),
            session_secret: real(get("SESSION_SECRET"))
                .filter(|s| {
                    let ok = s.len() >= MIN_SECRET;
                    if !ok {
                        eprintln!("SESSION_SECRET is shorter than {MIN_SECRET} bytes: auth disabled (use `openssl rand -base64 32`)");
                    }
                    ok
                })?,
            allowed_emails: parse_allowlist(&get("ALLOWED_EMAILS").unwrap_or_default()),
        })
    }

    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn allows(&self, email: &str) -> bool {
        self.allowed_emails.is_empty() || self.allowed_emails.contains(&email.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(extra: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let mut m: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://op.example/api/auth/"),
            ("OIDC_CLIENT_ID", "cid"),
            ("OIDC_CLIENT_SECRET", "csec"),
            ("PUBLIC_URL", "https://app.example/"),
            ("SESSION_SECRET", "0123456789abcdef0123456789abcdef"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in extra {
            m.insert(k.to_string(), v.to_string());
        }
        move |k| m.get(k).cloned()
    }

    #[test]
    fn enabled_when_all_set_and_trims_slashes() {
        let c = Oidc::from_lookup(env(&[])).unwrap();
        assert_eq!(c.issuer, "https://op.example/api/auth");
        assert_eq!(c.public_url, "https://app.example");
    }

    #[test]
    fn off_when_missing_or_placeholder() {
        assert!(Oidc::from_lookup(|_| None).is_none());
        for bad in ["...", "xxx", "XXX", "changeme", "  ", ""] {
            for k in [
                "OIDC_ISSUER",
                "OIDC_CLIENT_ID",
                "OIDC_CLIENT_SECRET",
                "PUBLIC_URL",
                "SESSION_SECRET",
            ] {
                assert!(Oidc::from_lookup(env(&[(k, bad)])).is_none(), "{k}={bad}");
            }
        }
    }

    #[test]
    fn short_session_secret_disables() {
        assert!(Oidc::from_lookup(env(&[("SESSION_SECRET", &"s".repeat(31))])).is_none());
        assert!(Oidc::from_lookup(env(&[("SESSION_SECRET", &"s".repeat(32))])).is_some());
    }

    #[test]
    fn allowlist() {
        assert_eq!(parse_allowlist(" A@x.io, ,b@x.io "), ["a@x.io", "b@x.io"]);
        assert!(Oidc::from_lookup(env(&[])).unwrap().allows("anyone@x.io"));
        let c = Oidc::from_lookup(env(&[("ALLOWED_EMAILS", "A@x.io")])).unwrap();
        assert!(c.allows("a@X.io"));
        assert!(!c.allows("b@x.io"));
    }
}
