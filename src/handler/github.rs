use log::info;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct GithubContent {
    pub name: String,
    pub download_url: Option<String>,
    #[serde(rename = "type")]
    pub content_type: String,
}

pub fn fetch_gitignore_templates(
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("USBackup-Agent")
        .build()?;

    info!("Fetching .gitignore templates from GitHub...");
    let resp: Vec<GithubContent> = client
        .get("https://api.github.com/repos/github/gitignore/contents")
        .send()?
        .json()?;

    let mut templates = Vec::new();
    for item in resp {
        if item.content_type == "file" && item.name.ends_with(".gitignore") {
            if let Some(url) = item.download_url {
                let display_name = item.name.trim_end_matches(".gitignore").to_string();
                templates.push((display_name, url));
            }
        }
    }
    Ok(templates)
}

pub fn download_gitignore(
    url: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("USBackup-Agent")
        .build()?;

    let content = client.get(url).send()?.text()?;
    let mut patterns = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            patterns.push(line.to_string());
        }
    }
    Ok(patterns)
}
