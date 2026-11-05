//! The web pages the app opens in the browser: the project's pages on GitHub (releases, issues, the
//! repository) and the website (the help page's links, 「在网站上查看」). open_external opens nothing
//! else. (Was apps/electron/src/shared/links.ts; app/shared/links.ts is the page's copy of the list.)

/// The project's pages on GitHub.
pub const PROJECT_PAGES: &str = "https://github.com/DuskXi/SpaceKnife/";
/// The website, with the documentation under /docs/ and /zh/docs/.
pub const WEBSITE: &str = "https://spaceknife.kaltsit.dev/";

/// The address to open when `url` is one of the project's pages (normalised), else None.
pub fn project_page(url: &str) -> Option<String> {
    // resolves "..", lowercases the host, percent-encodes as a browser does
    let href = url::Url::parse(url).ok()?.to_string();
    if href.starts_with(PROJECT_PAGES) || href.starts_with(WEBSITE) {
        return Some(href);
    }
    // the repository itself, as written in links: …/SpaceKnife and …/SpaceKnife#readme
    let repository = &PROJECT_PAGES[..PROJECT_PAGES.len() - 1];
    (href == repository || href.starts_with(&format!("{repository}#"))).then_some(href)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_projects_pages_on_github() {
        let page = "https://github.com/DuskXi/SpaceKnife/releases/tag/v0.4.0";
        assert_eq!(project_page(page).as_deref(), Some(page));
        assert_eq!(project_page("https://GitHub.com/DuskXi/SpaceKnife/releases").as_deref(), Some("https://github.com/DuskXi/SpaceKnife/releases"));
        // the repository itself, as the documentation links to it
        assert_eq!(project_page("https://github.com/DuskXi/SpaceKnife").as_deref(), Some("https://github.com/DuskXi/SpaceKnife"));
        assert_eq!(project_page("https://github.com/DuskXi/SpaceKnife#readme").as_deref(), Some("https://github.com/DuskXi/SpaceKnife#readme"));
    }

    #[test]
    fn opens_the_websites_pages() {
        assert_eq!(
            project_page("https://spaceknife.kaltsit.dev/zh/docs/filtering/#大小").as_deref(),
            Some("https://spaceknife.kaltsit.dev/zh/docs/filtering/#%E5%A4%A7%E5%B0%8F")
        );
        assert_eq!(project_page("https://SpaceKnife.Kaltsit.dev").as_deref(), Some("https://spaceknife.kaltsit.dev/"));
        assert_eq!(project_page("https://spaceknife.kaltsit.dev/docs/../").as_deref(), Some("https://spaceknife.kaltsit.dev/"));
    }

    #[test]
    fn refuses_everything_else() {
        for url in [
            "https://github.com/DuskXi/SpaceKnife?tab=evil",
            "https://github.com/DuskXi/SpaceKnife/../../evil/repo",
            "https://github.com/DuskXi/SpaceKnifeEvil/releases",
            "https://github.com.evil.example/DuskXi/SpaceKnife/",
            "https://user@github.com/DuskXi/SpaceKnife/releases",
            "http://github.com/DuskXi/SpaceKnife/releases",
            "http://spaceknife.kaltsit.dev/docs/",
            "https://spaceknife.kaltsit.dev.evil.example/docs/",
            "https://evil.example/?https://spaceknife.kaltsit.dev/",
            "https://user@spaceknife.kaltsit.dev/docs/",
            "https://spaceknife.kaltsit.dev:8443/docs/",
            "file:///C:/Windows/System32/calc.exe",
            "not a url",
            "",
        ] {
            assert_eq!(project_page(url), None, "{url}");
        }
    }
}
