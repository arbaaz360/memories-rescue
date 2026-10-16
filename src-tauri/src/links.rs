//! Every outside address the app shows or calls, read at compile time from
//! `src/config/links.json`, the same file the window's mock imports. One
//! place to change when the site publishes its pages.

use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Links {
    pub website: String,
    pub privacy: String,
    pub terms: String,
    pub faq: String,
    pub ffmpeg_source: String,
    pub support_email: String,
    pub gumroad_product: String,
    pub update_feed_stable: String,
    pub update_feed_beta: String,
}

pub static LINKS: LazyLock<Links> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../src/config/links.json"))
        .expect("src/config/links.json is valid")
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_https_and_the_stable_feed_matches_tauri_conf() {
        let l = &*LINKS;
        for url in [
            &l.website,
            &l.privacy,
            &l.terms,
            &l.faq,
            &l.ffmpeg_source,
            &l.gumroad_product,
            &l.update_feed_stable,
            &l.update_feed_beta,
        ] {
            assert!(url.starts_with("https://"), "{url}");
        }
        assert!(l.support_email.contains('@'));
        assert!(l.update_feed_beta.ends_with("latest-beta.json"));
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let endpoints = &conf["plugins"]["updater"]["endpoints"];
        assert_eq!(endpoints[0].as_str(), Some(l.update_feed_stable.as_str()));
        assert!(
            conf["plugins"]["updater"]["pubkey"]
                .as_str()
                .is_some_and(|k| k.len() > 40)
        );
    }
}
